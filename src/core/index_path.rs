use smallvec::SmallVec;
use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub struct IndexPath {
    pub indices: SmallVec<[usize; 8]>,
}

impl IndexPath {
    pub fn new(indices: SmallVec<[usize; 8]>) -> IndexPath {
        IndexPath { indices }
    }

    pub fn adding(&self, index: usize) -> IndexPath {
        let mut indices = self.indices.clone();
        indices.push(index);
        IndexPath::new(indices)
    }

    pub fn push(&mut self, index: usize) {
        self.indices.push(index);
    }

    pub fn increment_last(&mut self) {
        debug_assert!(!self.indices.is_empty(), "IndexPath::increment_last called on empty indices");
        if let Some(last) = self.indices.last_mut() {
            *last += 1;
        }
    }

    pub fn pop(&mut self) -> Option<usize> {
        self.indices.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

impl PartialEq for IndexPath {
    fn eq(&self, o: &Self) -> bool {
        self.indices.eq(&o.indices)
    }
}

impl Eq for IndexPath {}

impl PartialOrd for IndexPath {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        o.indices.partial_cmp(&self.indices)
    }
}

impl Ord for IndexPath {
    fn cmp(&self, o: &Self) -> Ordering {
        o.indices.cmp(&self.indices)
    }
}
