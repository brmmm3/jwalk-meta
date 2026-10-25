//! Ordered queue backed by a channel.
//!
//! 完成判定基于 OrderedMatcher（DFS 前序）+ channel disconnect，不依赖手动
//! pending_count。原先的 AtomicUsize 影子计数与 channel send 之间存在 race
//! 窗口（producer send 成功但 fetch_add 之前 consumer 已 drain 并 fetch_sub），
//! 导致 usize 下溢到 MAX。即便补救 fetch_add 也会在并发交错下制造虚假的
//! 非零计数，且该计数在 Strict 模式下从不被读取——纯 UB 源 + 死代码。

use crossbeam::channel::{self, Receiver, SendError, Sender, TryRecvError};
use smallvec::smallvec;
use std::collections::BinaryHeap;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// BinaryHeap 初始容量：256 是经验值，平衡初始内存占用和重新分配次数。
const INITIAL_HEAP_CAPACITY: usize = 256;

/// Strict 模式等待队头元素的最大时长，超时后返回 Empty 让出线程重试。
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
        // Acquire: 确保看到 stop 标志的最新值。
        self.stop.load(AtomicOrdering::Acquire)
    }

    /// 批量 drain channel 中所有就绪元素到 receive_buffer。
    fn drain_channel(&mut self) {
        while self.receive_buffer.len() < self.max_receive_buffer_size {
            if let Ok(ordered) = self.receiver.try_recv() {
                self.receive_buffer.push(ordered);
            } else {
                break;
            }
        }
    }

    /// Strict 模式：优先等待队头（looking_for），超时后返回 Empty 让出线程重试。
    ///
    /// 仅在堆中无目标元素时才 drain channel，减少堆膨胀。超时不弹非目标元素，
    /// 避免 OrderedMatcher 状态错乱；producer 最终会 push 目标元素或遍历完成
    /// 触发 channel disconnect。
    fn try_next_strict(&mut self) -> Result<Ordered<T>, TryRecvError> {
        let deadline = Instant::now() + STRICT_WAIT_TIMEOUT;

        loop {
            if self.is_stop() {
                return Err(TryRecvError::Disconnected);
            }

            // 仅当堆顶不是目标元素时才 drain channel，减少堆膨胀
            let looking_for = &self.ordered_matcher.looking_for;
            let top_matches = self.receive_buffer
                .peek()
                .map_or(false, |top| top.index_path.eq(looking_for));
            if !top_matches {
                self.drain_channel();
            }

            // 检查 buffer 中是否有目标元素
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
                // 超时：返回 Empty 让 yield 重试，不破坏 matcher 状态。
                return Err(TryRecvError::Empty);
            }

            // 带超时的阻塞等待
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
                    // 优先返回 looking_for 以保持正确性
                    if let Some(top) = self.receive_buffer.peek() {
                        if top.index_path.eq(&self.ordered_matcher.looking_for) {
                            let ordered = self.receive_buffer.pop().unwrap();
                            self.ordered_matcher.advance_past(&ordered);
                            return Ok(ordered);
                        }
                    }
                    // channel 断开后按堆序排空剩余元素（DFS 自然序）
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
