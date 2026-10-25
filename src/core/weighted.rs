//! Weighted wrapper for priority queue scheduling.

use super::IndexPath;
use std::cmp::Ordering;

/// Wrapper that orders by weight (descending) then index_path (ascending).
/// BinaryHeap is a max-heap, so higher weight items pop first.
pub struct Weighted<T> {
    pub value: T,
    pub index_path: IndexPath,
    pub weight: usize,
}

impl<T> Weighted<T> {
    pub fn new(value: T, index_path: IndexPath, weight: usize) -> Self {
        Weighted {
            value,
            index_path,
            weight,
        }
    }
}

impl<T> PartialEq for Weighted<T> {
    fn eq(&self, other: &Self) -> bool {
        self.weight == other.weight && self.index_path == other.index_path
    }
}

impl<T> Eq for Weighted<T> {}

impl<T> PartialOrd for Weighted<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Weighted<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher weight first (descending)
        match other.weight.cmp(&self.weight) {
            Ordering::Equal => {
                // Lower index_path first (ascending) for DFS order
                self.index_path.cmp(&other.index_path)
            }
            ord => ord,
        }
    }
}
