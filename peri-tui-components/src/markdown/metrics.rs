//! 解析路径的观测计数（thread-local，测试与宿主诊断共用）。
//!
//! 计数在解析入口无条件累加，宿主的 `perf_counters()` 只做快照合并；这样
//! 依赖本 crate 的测试（如 entry 缓存与子代理详情）仍能断言解析是否发生。

use std::cell::Cell;

/// Markdown 管线计数快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkdownMetrics {
    pub full_parses: u64,
    pub full_parsed_bytes: u64,
    pub tail_parses: u64,
    pub tail_parsed_bytes: u64,
    pub materialized_lines: u64,
}

impl MarkdownMetrics {
    fn add(&mut self, counter: MarkdownCounter, value: u64) {
        match counter {
            MarkdownCounter::FullParse => self.full_parses += value,
            MarkdownCounter::FullParsedBytes => self.full_parsed_bytes += value,
            MarkdownCounter::TailParse => self.tail_parses += value,
            MarkdownCounter::TailParsedBytes => self.tail_parsed_bytes += value,
            MarkdownCounter::MaterializedLines => self.materialized_lines += value,
        }
    }
}

/// 可观测的解析事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownCounter {
    FullParse,
    FullParsedBytes,
    TailParse,
    TailParsedBytes,
    MaterializedLines,
}

thread_local! {
    static METRICS: Cell<MarkdownMetrics> = Cell::new(MarkdownMetrics::default());
}

/// 累加一个事件。
pub fn observe(counter: MarkdownCounter, value: u64) {
    METRICS.with(|metrics| {
        let mut snapshot = metrics.get();
        snapshot.add(counter, value);
        metrics.set(snapshot);
    });
}

/// 读取当前线程的计数快照。
#[must_use]
pub fn snapshot() -> MarkdownMetrics {
    METRICS.with(Cell::get)
}

/// 清零当前线程的计数。
pub fn reset() {
    METRICS.with(|metrics| metrics.set(MarkdownMetrics::default()));
}
