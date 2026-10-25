use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::{
    ClientState, Ordered, OrderedQueue, PriorityQueue, ReadDir, ReadDirCallback, ReadDirSpec,
    Weighted,
};
use crate::Result;

pub(crate) struct RunContext<C: ClientState> {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) read_dir_spec_queue: PriorityQueue<ReadDirSpec<C>>,
    pub(crate) read_dir_result_queue: OrderedQueue<Result<ReadDir<C>>>,
    pub(crate) core_read_dir_callback: Arc<ReadDirCallback<C>>,
}

impl<C: ClientState> RunContext<C> {
    pub(crate) fn schedule_read_dir_spec(
        &self,
        weighted_read_dir: Weighted<ReadDirSpec<C>>,
    ) -> bool {
        self.read_dir_spec_queue.push(weighted_read_dir).is_ok()
    }

    pub(crate) fn send_read_dir_result(
        &self,
        read_dir_result: Ordered<Result<ReadDir<C>>>,
    ) -> bool {
        self.read_dir_result_queue.push(read_dir_result).is_ok()
    }

    pub(crate) fn complete_item(&self) {
        self.read_dir_spec_queue.complete_item();
    }
}

impl<C: ClientState> Clone for RunContext<C> {
    fn clone(&self) -> Self {
        RunContext {
            stop: self.stop.clone(),
            read_dir_spec_queue: self.read_dir_spec_queue.clone(),
            read_dir_result_queue: self.read_dir_result_queue.clone(),
            core_read_dir_callback: self.core_read_dir_callback.clone(),
        }
    }
}
