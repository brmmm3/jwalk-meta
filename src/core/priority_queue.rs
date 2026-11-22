//! Priority queue backed by a channel and BinaryHeap.

use crossbeam::channel::{self, Receiver, SendError, Sender, TryRecvError};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use super::*;

pub(crate) struct PriorityQueue<T>
where
    T: Send,
{
    sender: Sender<Weighted<T>>,
    pending_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

pub struct PriorityQueueIter<T>
where
    T: Send,
{
    stop: Arc<AtomicBool>,
    receiver: Receiver<Weighted<T>>,
    receive_buffer: BinaryHeap<Weighted<T>>,
    pending_count: Arc<AtomicUsize>,
}

/// Initial BinaryHeap capacity: 256 is an empirical balance between memory use and reallocations.
/// Most directories have fewer than 256 children, avoiding the first growth allocation.
const INITIAL_HEAP_CAPACITY: usize = 256;

pub(crate) fn new_priority_queue<T>(
    stop: Arc<AtomicBool>,
    _channel_capacity: usize,
) -> (PriorityQueue<T>, PriorityQueueIter<T>)
where
    T: Send,
{
    let pending_count = Arc::new(AtomicUsize::new(0));
    // Workers produce more work than they consume, so blocking them on a full
    // work queue can prevent the only consumer from ever running.
    let (sender, receiver) = channel::unbounded();
    (
        PriorityQueue {
            sender,
            pending_count: pending_count.clone(),
            stop: stop.clone(),
        },
        PriorityQueueIter {
            receiver,
            receive_buffer: BinaryHeap::with_capacity(INITIAL_HEAP_CAPACITY),
            pending_count,
            stop,
        },
    )
}

impl<T> PriorityQueue<T>
where
    T: Send,
{
    pub fn push(&self, weighted: Weighted<T>) -> Result<(), SendError<Weighted<T>>> {
        // fetch_add must precede send to eliminate a false pending_count==0 during the send->fetch_add window.
        // Before the fix (send before fetch_add), the consumer could drain between the two operations, see
        // pending_count==0, and return Disconnected early, stopping par_bridge and losing subsequent elements.
        // With the fix (fetch_add before send), pending_count is >= 1 when an element enters the channel,
        // so the consumer cannot misjudge it. If send fails, fetch_sub rolls back the count.
        self.pending_count.fetch_add(1, AtomicOrdering::Release);
        match self.sender.send(weighted) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.pending_count.fetch_sub(1, AtomicOrdering::Release);
                Err(e)
            }
        }
    }

    pub fn complete_item(&self) {
        // AcqRel: consumer-side try_next reads via load(Acquire); the pair guarantees visibility.
        // fetch_add preceding send prevents underflow (complete_item is called only after an element
        // has been consumed by a consumer and processed by a worker, when pending_count must be >= 1).
        self.pending_count.fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

impl<T> Clone for PriorityQueue<T>
where
    T: Send,
{
    fn clone(&self) -> Self {
        PriorityQueue {
            sender: self.sender.clone(),
            pending_count: self.pending_count.clone(),
            stop: self.stop.clone(),
        }
    }
}

impl<T> PriorityQueueIter<T>
where
    T: Send,
{
    fn pending_count(&self) -> usize {
        // Acquire: pairs with the Release in push to ensure complete data writes are visible.
        self.pending_count.load(AtomicOrdering::Acquire)
    }

    fn is_stop(&self) -> bool {
        // Acquire: ensure the latest stop flag written with Release by the producer is observed.
        self.stop.load(AtomicOrdering::Acquire)
    }

    fn try_next(&mut self) -> Result<Weighted<T>, TryRecvError> {
        // Adaptive backoff: short waits (10 μs) on the hot path, gradually increasing to 1 ms.
        // Avoid wasting CPU while idle while still responding quickly to new data.
        let mut spin_count: u32 = 0;

        loop {
            if self.is_stop() {
                return Err(TryRecvError::Disconnected);
            }

            // First, non-blockingly drain all ready elements.
            while let Ok(weighted) = self.receiver.try_recv() {
                self.receive_buffer.push(weighted);
                spin_count = 0; // Reset backoff count when data is available.
            }

            if let Some(weighted) = self.receive_buffer.pop() {
                return Ok(weighted);
            } else if self.pending_count() == 0 {
                return Err(TryRecvError::Disconnected);
            }

            // Adaptive wait: gradually increase the timeout to reduce idle CPU usage.
            let timeout = match spin_count {
                0..=10 => std::time::Duration::from_micros(10),
                11..=50 => std::time::Duration::from_micros(100),
                _ => std::time::Duration::from_millis(1),
            };
            spin_count += 1;

            match self.receiver.recv_timeout(timeout) {
                Ok(weighted) => {
                    self.receive_buffer.push(weighted);
                }
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => {
                    return Err(TryRecvError::Disconnected);
                }
            }
        }
    }
}

impl<T> Iterator for PriorityQueueIter<T>
where
    T: Send,
{
    type Item = Weighted<T>;

    fn next(&mut self) -> Option<Weighted<T>> {
        loop {
            match self.try_next() {
                Ok(next) => return Some(next),
                Err(TryRecvError::Empty) => {
                    std::thread::yield_now();
                }

                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    #[test]
    fn disconnected_receiver_rolls_back_pending_count() {
        let stop = Arc::new(AtomicBool::new(false));
        let (queue, receiver) = new_priority_queue(stop, 1);
        drop(receiver);
        let result = queue.push(Weighted::new(1, IndexPath::new(smallvec![0]), 1));
        assert!(result.is_err());
        assert_eq!(queue.pending_count.load(AtomicOrdering::Acquire), 0);
    }

    #[test]
    fn disconnected_producer_ends_iteration_with_pending_work() {
        let stop = Arc::new(AtomicBool::new(false));
        let (queue, mut receiver) = new_priority_queue(stop, 1);
        queue
            .push(Weighted::new(1, IndexPath::new(smallvec![0]), 1))
            .unwrap();
        assert_eq!(receiver.next().unwrap().value, 1);
        drop(queue);
        assert!(receiver.next().is_none());
    }

    #[test]
    fn equal_weight_tasks_prioritize_earlier_paths() {
        use std::cmp::Ordering;
        let first = IndexPath::new(smallvec![0, 0]);
        let second = IndexPath::new(smallvec![0, 1]);
        assert_eq!(first.partial_cmp(&second), Some(Ordering::Greater));
        let first = Weighted::new("first", first, 3);
        let second = Weighted::new("second", second, 3);
        assert!(first != second);
        assert!(first == Weighted::new("other value", IndexPath::new(smallvec![0, 0]), 3));
        assert_eq!(first.partial_cmp(&second), Some(Ordering::Greater));
        let mut heap = BinaryHeap::from(vec![second, first]);
        assert_eq!(heap.pop().unwrap().value, "first");
        assert_eq!(heap.pop().unwrap().value, "second");
    }

    #[test]
    fn stop_interrupts_an_empty_queue_with_pending_work() {
        let stop = Arc::new(AtomicBool::new(false));
        let (queue, mut receiver) = new_priority_queue(stop.clone(), 1);
        queue
            .push(Weighted::new(1, IndexPath::new(smallvec![0]), 1))
            .unwrap();
        assert_eq!(receiver.next().unwrap().value, 1);
        stop.store(true, AtomicOrdering::Release);
        assert!(receiver.next().is_none());
        queue.complete_item();
    }
}
