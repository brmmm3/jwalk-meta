use super::{ClientState, DirEntry, IndexPath, ReadDirSpec, Weighted};
use crate::Result;

/// Results of successfully reading a directory.
#[derive(Debug)]
pub struct ReadDir<C: ClientState> {
    pub(crate) read_dir_state: C::ReadDirState,
    pub(crate) results_list: Vec<Result<DirEntry<C>>>,
    /// Number of subdirectories scheduled early during streaming dispatch.
    pub(crate) streamed_child_count: usize,
}

impl<C: ClientState> ReadDir<C> {
    pub fn new(
        read_dir_state: C::ReadDirState,
        results_list: Vec<Result<DirEntry<C>>>,
    ) -> ReadDir<C> {
        ReadDir {
            read_dir_state,
            results_list,
            streamed_child_count: 0,
        }
    }

    pub fn read_children_specs(&self) -> impl Iterator<Item = ReadDirSpec<C>> + '_ {
        self.results_list
            .iter()
            .filter_map(move |each| each.as_ref().ok()?.read_children_spec(&self.read_dir_state))
    }

    /// Single-pass traversal: generate Weighted<ReadDirSpec> directly, skipping subdirectories already scheduled during streaming.
    /// Priority-flooding algorithm: weight = parent_weight + pipe_size (the total number of entries in the parent directory).
    /// Inheriting the weight ensures that branches of large pipelines also get higher priority.
    pub fn weighted_children_specs(
        &self,
        index_path: &IndexPath,
        parent_weight: usize,
    ) -> Vec<Weighted<ReadDirSpec<C>>> {
        let skip = self.streamed_child_count;
        let pipe_size = self.results_list.len();
        let weight = parent_weight.saturating_add(pipe_size);

        // Single-pass traversal: skip the portion already streamed and construct Weighted<ReadDirSpec> directly.
        // Increment subdir_counter only for subdirectories that produce a spec,
        // ensuring a contiguous IndexPath sequence (0,1,2,...) instead of gaps from enumerate indices.
        let mut specs = Vec::with_capacity(pipe_size.saturating_sub(skip));
        let mut subdir_counter = 0usize;
        for entry_result in self.results_list.iter() {
            if let Some(spec) = entry_result
                .as_ref()
                .ok()
                .and_then(|e| e.read_children_spec(&self.read_dir_state))
            {
                if subdir_counter >= skip {
                    specs.push(Weighted::new(
                        spec,
                        index_path.adding(subdir_counter),
                        weight,
                    ));
                }
                subdir_counter += 1;
            }
        }
        specs
    }
}
