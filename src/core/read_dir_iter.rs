use smallvec::smallvec;

use super::*;
use crate::Result;
use std::sync::atomic::Ordering as AtomicOrdering;

/// Client's read dir function.
///
/// The second argument, `Option<StreamingContext<C>>`, provides streaming-dispatch context in parallel mode,
/// allowing subdirectories to be scheduled immediately during enumeration instead of waiting for enumeration to finish.
/// Pass `None` in single-threaded Walk mode.
pub(crate) type ReadDirCallback<C> =
    dyn Fn(ReadDirSpec<C>, Option<StreamingContext<C>>) -> Result<ReadDir<C>> + Send + Sync + 'static;

/// Streaming-dispatch context for immediately scheduling subdirectories during NT enumeration.
///
/// Created in `multi_threaded_walk_dir` and passed to the callback.
/// The callback calls `schedule()` for each subdirectory to schedule it immediately.
pub(crate) struct StreamingContext<C: ClientState> {
    pub index_path: IndexPath,
    run_context: RunContext<C>,
    /// Parent directory's scheduling weight, used for weight inheritance in the priority-flooding algorithm.
    /// Subdirectory weight = parent_weight + number of discovered subdirectories,
    /// ensuring that branches of large pipelines (many subdirectories) also get high priority.
    pub parent_weight: usize,
}

impl<C: ClientState> StreamingContext<C> {
    pub(crate) fn new(index_path: IndexPath, run_context: RunContext<C>, parent_weight: usize) -> Self {
        StreamingContext {
            index_path,
            run_context,
            parent_weight,
        }
    }

    /// Schedule a subdirectory read task in the priority queue.
    /// Return true if enqueued successfully, or false if the queue is closed (traversal has stopped).
    pub(crate) fn schedule(&self, weighted: Weighted<ReadDirSpec<C>>) -> bool {
        self.run_context.schedule_read_dir_spec(weighted)
    }

    /// Check whether traversal has been stopped (close() was called).
    pub(crate) fn is_stopped(&self) -> bool {
        self.run_context.stop.load(AtomicOrdering::Acquire)
    }
}

/// Result<ReadDir> Iterator.
///
/// Yields ReadDirs (results of fs::read_dir) in order required for recursive
/// directory traversal. Depending on Walk/ParWalk state these reads might be
/// computed in parallel.
pub enum ReadDirIter<C: ClientState> {
    Walk {
        read_dir_spec_stack: Vec<ReadDirSpec<C>>,
        core_read_dir_callback: Arc<ReadDirCallback<C>>,
    },
    ParWalk {
        read_dir_result_iter: OrderedQueueIter<Result<ReadDir<C>>>,
    },
}

impl<C: ClientState> ReadDirIter<C> {
    pub(crate) fn try_new(
        read_dir_specs: Vec<ReadDirSpec<C>>,
        parallelism: Parallelism,
        core_read_dir_callback: Arc<ReadDirCallback<C>>,
        channel_capacity: usize,
        max_receive_buffer_size: usize,
    ) -> Option<(Self, Option<Arc<AtomicBool>>)> {
        if let Parallelism::Serial = parallelism {
            Some((
                ReadDirIter::Walk {
                    read_dir_spec_stack: read_dir_specs,
                    core_read_dir_callback,
                },
                None,
            ))
        } else {
            let stop = Arc::new(AtomicBool::new(false));
            let read_dir_result_queue =
                new_ordered_queue(stop.clone(), channel_capacity, max_receive_buffer_size);
            let (read_dir_result_queue, read_dir_result_iter) = read_dir_result_queue;
            let (read_dir_spec_queue, read_dir_spec_iter) =
                new_priority_queue(stop.clone(), channel_capacity);

            // Give the root a high weight so it is scheduled first (priority-flooding algorithm).
            // Use (usize::MAX >> 1) to avoid overflow in parent_weight + child_count.
            const ROOT_WEIGHT: usize = usize::MAX >> 1;
            for read_dir_spec in read_dir_specs.into_iter() {
                // The channel cannot be full during initialization (it was just created), but use expect to make the invariant explicit.
                read_dir_spec_queue
                    .push(Weighted::new(read_dir_spec, IndexPath::new(smallvec![0]), ROOT_WEIGHT))
                    .expect("init: priority queue push should not fail");
            }

            let run_context = RunContext {
                stop: stop.clone(),
                read_dir_spec_queue,
                read_dir_result_queue,
                core_read_dir_callback,
            };

            let (startup_tx, startup_rx) = parallelism
                .timeout()
                .map(|duration| {
                    let (tx, rx) = crossbeam::channel::unbounded();
                    (Some(tx), Some((rx, duration)))
                })
                .unwrap_or((None, None));
            parallelism.spawn(move || {
                if let Some(tx) = startup_tx {
                    if tx.send(()).is_err() {
                        return;
                    }
                }
                read_dir_spec_iter.par_bridge().for_each_with(
                    run_context,
                    |run_context, weighted_read_dir_spec| {
                        multi_threaded_walk_dir(weighted_read_dir_spec, run_context);
                    },
                );
            });
            if startup_rx.is_some_and(|(rx, duration)| rx.recv_timeout(duration).is_err()) {
                // Busy timeout triggered: notify already-spawned threads to stop,
                // preventing orphaned threads from continuing to traverse the entire directory tree.
                stop.store(true, AtomicOrdering::Release);
                return None;
            }
            Some((
                ReadDirIter::ParWalk {
                    read_dir_result_iter,
                },
                Some(stop),
            ))
        }
    }
}

impl<C: ClientState> Iterator for ReadDirIter<C> {
    type Item = Result<ReadDir<C>>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            ReadDirIter::Walk {
                read_dir_spec_stack,
                core_read_dir_callback,
            } => {
                let read_dir_spec = read_dir_spec_stack.pop()?;
                // Walk mode does not use streaming dispatch.
                let read_dir_result = core_read_dir_callback(read_dir_spec, None);

                if let Ok(read_dir) = read_dir_result.as_ref() {
                    let mut specs = read_dir.read_children_specs().collect::<Vec<_>>();
                    specs.reverse();
                    for each_spec in specs {
                        read_dir_spec_stack.push(each_spec);
                    }
                }

                Some(read_dir_result)
            }

            ReadDirIter::ParWalk {
                read_dir_result_iter,
            } => read_dir_result_iter
                .next()
                .map(|read_dir_result| read_dir_result.value),
        }
    }
}

fn multi_threaded_walk_dir<C: ClientState>(
    weighted_read_dir_spec: Weighted<ReadDirSpec<C>>,
    run_context: &mut RunContext<C>,
) {
    if run_context.stop.load(AtomicOrdering::Acquire) {
        run_context.read_dir_spec_queue.complete_item();
        return;
    }

    let Weighted {
        value: read_dir_spec,
        index_path,
        weight: parent_weight,
    } = weighted_read_dir_spec;

    let streaming_ctx = StreamingContext::new(index_path.clone(), run_context.clone(), parent_weight);

    let read_dir_result = (run_context.core_read_dir_callback)(read_dir_spec, Some(streaming_ctx));

    // Streaming callbacks may discover only a subset of traversable children (for example,
    // followed directory symlinks). Always schedule the remaining specs; ReadDir skips the
    // first `streamed_child_count` children which were already enqueued.
    let schedule_regular = read_dir_result.is_ok();

    let weighted_children_specs = if schedule_regular {
        read_dir_result
            .as_ref()
            .ok()
            .map(|read_dir| read_dir.weighted_children_specs(&index_path, parent_weight))
    } else {
        None
    };

    let streamed_count = read_dir_result.as_ref().map_or(0, |rd| rd.streamed_child_count);
    let regular_count = weighted_children_specs.as_ref().map_or(0, Vec::len);
    let child_count = streamed_count + regular_count;

    let ordered_read_dir_result = Ordered::new(
        read_dir_result,
        index_path,
        child_count,
    );

    let send_ok = run_context.send_read_dir_result(ordered_read_dir_result);

    if send_ok {
        if let Some(weighted_children_specs) = weighted_children_specs {
            for each in weighted_children_specs {
                if !run_context.schedule_read_dir_spec(each) {
                    break;
                }
            }
        }
    }

    if send_ok {
        run_context.complete_item();
    } else {
        run_context.read_dir_spec_queue.complete_item();
    }
}
