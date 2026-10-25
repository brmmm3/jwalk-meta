//! Ordered queue backed by a channel.
//!
//! Completion is determined by OrderedMatcher (DFS preorder) and channel disconnect, not manual bookkeeping.
//! pending_count. The previous AtomicUsize shadow counter had a race window with channel send
//! (the producer could send successfully, then the consumer could drain and fetch_sub before fetch_add),
//! causing usize to underflow to MAX. Even adding a compensating fetch_add could create a false
//! nonzero count under concurrent interleavings, and the count was never read in Strict mode—pure UB risk and dead code.

use crossbeam::channel::{self, Receiver, SendError, Sender, TryRecvError};
use smallvec::smallvec;
use std::collections::BinaryHeap;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// Initial BinaryHeap capacity: 256 is an empirical balance between memory use and reallocations.
const INITIAL_HEAP_CAPACITY: usize = 256;

/// Maximum time Strict mode waits for the head element before returning Empty and yielding for a retry.
const STRICT_WAIT_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) struct OrderedQueue<T>
where
    T: Send,
{
    sender: Sender<Ordered<T>>,
    stop: Arc<AtomicBool>,
}

pub struct OrderedQueueIter<T>
where
    T: Send,
{
    stop: Arc<AtomicBool>,
    receiver: Receiver<Ordered<T>>,
    receive_buffer: BinaryHeap<Ordered<T>>,
    ordered_matcher: OrderedMatcher,
    max_receive_buffer_size: usize,
}

struct OrderedMatcher {
    looking_for: IndexPath,
    child_count_stack: Vec<usize>,
}

pub(crate) fn new_ordered_queue<T>(
    stop: Arc<AtomicBool>,
    channel_capacity: usize,
    max_receive_buffer_size: usize,
) -> (OrderedQueue<T>, OrderedQueueIter<T>)
where
    T: Send,
{
    let (sender, receiver) = channel::bounded(channel_capacity);
    (
        OrderedQueue {
            sender,
            stop: stop.clone(),
        },
        OrderedQueueIter {
            receiver,
            ordered_matcher: OrderedMatcher::default(),
            receive_buffer: BinaryHeap::with_capacity(INITIAL_HEAP_CAPACITY),
            stop,
            max_receive_buffer_size,
        },
    )
}

impl<T> OrderedQueue<T>
where
    T: Send,
{
    pub fn push(&self, ordered: Ordered<T>) -> Result<(), SendError<Ordered<T>>> {
        self.sender.send(ordered)
    }
}

impl<T> Clone for OrderedQueue<T>
where
    T: Send,
{
    fn clone(&self) -> Self {
        OrderedQueue {
            sender: self.sender.clone(),
            stop: self.stop.clone(),
        }
    }
}

impl<T> OrderedQueueIter<T>
where
    T: Send,
{
    fn is_stop(&self) -> bool {
        // Acquire: ensure the latest stop flag is observed.
        self.stop.load(AtomicOrdering::Acquire)
    }

    /// Drain all ready elements from the channel into receive_buffer.
    fn drain_channel(&mut self) {
        while self.receive_buffer.len() < self.max_receive_buffer_size {
            if let Ok(ordered) = self.receiver.try_recv() {
                self.receive_buffer.push(ordered);
            } else {
                break;
            }
        }
    }

    /// Strict mode: prioritize waiting for the head element (looking_for), then return Empty on timeout and yield for a retry.
    ///
    /// Drain the channel only when the target is not in the heap, reducing heap growth. Do not pop a non-target element on timeout,
    /// which would leave OrderedMatcher in an inconsistent state; the producer will eventually push the target or finish traversal,
    /// disconnecting the channel.
    fn try_next_strict(&mut self) -> Result<Ordered<T>, TryRecvError> {
        let deadline = Instant::now() + STRICT_WAIT_TIMEOUT;

        loop {
            if self.is_stop() {
                return Err(TryRecvError::Disconnected);
            }

            // Drain the channel only if the heap head is not the target, reducing heap growth.
            let looking_for = &self.ordered_matcher.looking_for;
            let top_matches = self
                .receive_buffer
                .peek()
                .is_some_and(|top| top.index_path.eq(looking_for));
            if !top_matches {
                self.drain_channel();
            }

            // Check whether the buffer contains the target element.
            let looking_for = &self.ordered_matcher.looking_for;
            if let Some(top_ordered) = self.receive_buffer.peek() {
                if top_ordered.index_path.eq(looking_for) {
                    break;
                }
            }

            if self.ordered_matcher.is_none() {
                return Err(TryRecvError::Disconnected);
            }

            if Instant::now() >= deadline {
                // Timeout: return Empty and let yield retry without disturbing matcher state.
                return Err(TryRecvError::Empty);
            }

            // Blocking wait with a timeout.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait_time = remaining.max(Duration::from_micros(100));
            match self.receiver.recv_timeout(wait_time) {
                Ok(ordered) => {
                    self.receive_buffer.push(ordered);
                }
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => {
                    self.drain_channel();
                    // Return looking_for first to preserve correctness.
                    if let Some(top) = self.receive_buffer.peek() {
                        if top.index_path.eq(&self.ordered_matcher.looking_for) {
                            let ordered = self.receive_buffer.pop().unwrap();
                            self.ordered_matcher.advance_past(&ordered);
                            return Ok(ordered);
                        }
                    }
                    // After channel disconnect, drain remaining elements in heap order (natural DFS order).
                    if let Some(fallback) = self.receive_buffer.pop() {
                        return Ok(fallback);
                    }
                    return Err(TryRecvError::Disconnected);
                }
            }
        }

        let ordered = self.receive_buffer.pop().unwrap();
        self.ordered_matcher.advance_past(&ordered);
        Ok(ordered)
    }
}

impl<T> Iterator for OrderedQueueIter<T>
where
    T: Send,
{
    type Item = Ordered<T>;
    fn next(&mut self) -> Option<Ordered<T>> {
        loop {
            match self.try_next_strict() {
                Ok(next) => return Some(next),
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

impl OrderedMatcher {
    fn is_none(&self) -> bool {
        self.looking_for.is_empty()
    }

    fn decrement_remaining_children(&mut self) {
        if let Some(count) = self.child_count_stack.last_mut() {
            *count = count.saturating_sub(1);
        }
    }

    fn advance_past<T>(&mut self, ordered: &Ordered<T>) {
        self.decrement_remaining_children();

        if ordered.child_count > 0 {
            self.looking_for.push(0);
            self.child_count_stack.push(ordered.child_count);
        } else {
            self.looking_for.increment_last();
            while !self.child_count_stack.is_empty() && *self.child_count_stack.last().unwrap() == 0
            {
                self.looking_for.pop();
                self.child_count_stack.pop();
                if !self.looking_for.is_empty() {
                    self.looking_for.increment_last();
                }
            }
        }
    }
}

impl Default for OrderedMatcher {
    fn default() -> OrderedMatcher {
        OrderedMatcher {
            looking_for: IndexPath::new(smallvec![0]),
            child_count_stack: vec![1],
        }
    }
}
