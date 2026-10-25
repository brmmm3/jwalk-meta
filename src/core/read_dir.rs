use super::{ClientState, DirEntry, IndexPath, Weighted, ReadDirSpec};
use crate::Result;

/// Results of successfully reading a directory.
#[derive(Debug)]
pub struct ReadDir<C: ClientState> {
    pub(crate) read_dir_state: C::ReadDirState,
    pub(crate) results_list: Vec<Result<DirEntry<C>>>,
    /// 流式分发期间已提前调度的子目录数量。
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
        self.results_list.iter().filter_map(move |each| {
            each.as_ref()
                .ok()?
                .read_children_spec(self.read_dir_state.clone())
        })
    }

    /// 单遍遍历：直接生成 Weighted<ReadDirSpec>，跳过已流式调度的子目录。
    /// 优先淹没算法：weight = parent_weight + pipe_size（父目录总条目数），
    /// 权重继承确保大管道的分支也获得高优先级。
    pub fn weighted_children_specs(
        &self,
        index_path: &IndexPath,
        parent_weight: usize,
    ) -> Vec<Weighted<ReadDirSpec<C>>> {
        let skip = self.streamed_child_count;
        let pipe_size = self.results_list.len();
        let weight = parent_weight.saturating_add(pipe_size);

        // 单遍遍历：跳过已流式部分，直接构造 Weighted<ReadDirSpec>
        // 使用 subdir_counter 仅对产生 spec 的子目录递增，
        // 确保 IndexPath 序列连续（0,1,2,...），而非按 enumerate 索引产生空洞。
        let mut specs = Vec::with_capacity(pipe_size.saturating_sub(skip));
        let mut subdir_counter = 0usize;
        for entry_result in self.results_list.iter() {
            if let Some(spec) = entry_result
                .as_ref()
                .ok()
                .and_then(|e| e.read_children_spec(self.read_dir_state.clone()))
            {
                if subdir_counter >= skip {
                    specs.push(Weighted::new(spec, index_path.adding(subdir_counter), weight));
                }
                subdir_counter += 1;
            }
        }
        specs
    }
}
