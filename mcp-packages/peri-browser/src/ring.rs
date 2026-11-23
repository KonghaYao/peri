//! Bounded ring buffer used by the per-page console and network diagnostics.
//!
//! Keeping the newest entries is the point: a busy page can emit thousands of
//! diagnostics, and the model only needs the tail. Dropping the oldest entry is
//! silent and cheap (README §7).

use std::collections::VecDeque;

/// Fixed-capacity newest-wins buffer.
#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    capacity: usize,
    entries: VecDeque<T>,
}

impl<T> RingBuffer<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, entry: T) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> {
        self.entries.iter_mut()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[must_use]
    pub fn last(&self) -> Option<&T> {
        self.entries.back()
    }
}

#[cfg(test)]
#[path = "ring_test.rs"]
mod tests;
