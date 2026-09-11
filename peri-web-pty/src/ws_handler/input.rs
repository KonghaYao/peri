//! 待写 PTY 输入的容量、部分写入进度与关闭状态。
use std::collections::VecDeque;

const INPUT_QUEUE_CAPACITY: usize = 16;

#[derive(Default)]
pub(super) struct InputQueue {
    frames: VecDeque<Vec<u8>>,
    written: usize,
    closed: bool,
}

impl InputQueue {
    /// False means overload: the connection must close, regardless of input origin.
    pub(super) fn enqueue(&mut self, bytes: Vec<u8>) -> bool {
        if self.closed || bytes.is_empty() {
            return true;
        }
        if self.frames.len() >= INPUT_QUEUE_CAPACITY {
            tracing::warn!("PTY input queue capacity exceeded");
            return false;
        }
        self.frames.push_back(bytes);
        true
    }

    pub(super) fn pending(&self) -> &[u8] {
        self.frames
            .front()
            .map(|bytes| &bytes[self.written..])
            .unwrap_or_default()
    }

    pub(super) fn is_open(&self) -> bool {
        !self.closed
    }

    pub(super) fn advance(&mut self, count: usize) {
        self.written += count;
        if self.written
            == self
                .frames
                .front()
                .expect("write requires queued input")
                .len()
        {
            self.frames.pop_front();
            self.written = 0;
        }
    }

    pub(super) fn close(&mut self) {
        self.closed = true;
        self.frames.clear();
        self.written = 0;
    }
}

#[cfg(test)]
#[path = "input_test.rs"]
mod tests;
