//! 有界事件日志：单调 `sequence` + 保留边界 + 分页读取（design §4.3）。
//!
//! 供 `workflow://runs/{runId}/events` 与 `workflow://tasks` 两类 event-log 资源使用。
//! 通知只是唤醒信号、不承担投递保证：读取方按游标补读，发现游标早于保留边界时
//! 重新建立快照（不假装连续）。

use std::collections::VecDeque;

use parking_lot::Mutex;

/// 单页读取结果：`headSequence` / `truncatedBefore` 随页返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogPage<T> {
    /// 按 sequence 升序的条目（至多 `limit` 条）。
    pub entries: Vec<T>,
    /// 已提交上界（无事件时为 0）。
    pub head_sequence: u64,
    /// 保留边界：`sequence < truncated_before` 的条目已不可读。
    pub truncated_before: u64,
}

/// 有界事件日志：容量满时淘汰最旧条目，保留边界随之前进。
pub struct BoundedEventLog<T> {
    inner: Mutex<Inner<T>>,
    capacity: usize,
}

struct Inner<T> {
    entries: VecDeque<(u64, T)>,
    /// 下一条事件的 sequence（从 1 起）。
    next: u64,
    truncated_before: u64,
}

impl<T> BoundedEventLog<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                next: 1,
                truncated_before: 1,
            }),
            capacity: capacity.max(1),
        }
    }

    /// 追加一条事件，返回其 sequence（单调递增，从 1 起）。
    pub fn append(&self, value: T) -> u64 {
        self.append_with(|_| value)
    }

    /// 追加一条事件：`make` 接收本次分配的 sequence 并构造值（供事件自带
    /// sequence 的场景；闭包在持锁期间执行，不得重入本日志）。
    pub fn append_with(&self, make: impl FnOnce(u64) -> T) -> u64 {
        let mut inner = self.inner.lock();
        let sequence = inner.next;
        inner.next += 1;
        inner.entries.push_back((sequence, make(sequence)));
        while inner.entries.len() > self.capacity {
            if let Some((evicted, _)) = inner.entries.pop_front() {
                inner.truncated_before = evicted + 1;
            }
        }
        sequence
    }

    /// 已提交上界（无事件时为 0）。
    pub fn head_sequence(&self) -> u64 {
        self.inner.lock().next.saturating_sub(1)
    }

    /// 保留边界：小于它的 sequence 已不可读。
    pub fn truncated_before(&self) -> u64 {
        self.inner.lock().truncated_before
    }

    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `after`（exclusive）之后至多 `limit` 条。
    ///
    /// `after` 早于保留边界（或为 `None`）时从保留边界起读：页内的
    /// `truncated_before` 让读取方识别「游标已失效」，自行重建快照。
    pub fn page(&self, after: Option<u64>, limit: usize) -> EventLogPage<T>
    where
        T: Clone,
    {
        self.page_where(after, limit, |_| true)
    }

    /// 带谓词的过滤分页（跨 run 聚合日志的单 run 视图）。
    pub fn page_where(
        &self,
        after: Option<u64>,
        limit: usize,
        predicate: impl Fn(&T) -> bool,
    ) -> EventLogPage<T>
    where
        T: Clone,
    {
        let inner = self.inner.lock();
        let start = match after {
            Some(cursor) => cursor.saturating_add(1).max(inner.truncated_before),
            None => inner.truncated_before,
        };
        // `partition_point` 依赖「entries 内 sequence 严格递增」这一既有不变量
        // （`append_with` 单调分配 sequence，淘汰只从队首进行）：因此首个
        // `sequence >= start` 的位置之前全部小于 `start`，二分定位与逐条
        // 前缀判断等价，但每页不再重扫已读前缀。
        let begin = inner
            .entries
            .partition_point(|(sequence, _)| *sequence < start);
        let entries = inner
            .entries
            .range(begin..)
            .map(|(_, value)| value)
            .filter(|value| predicate(value))
            .take(limit)
            .cloned()
            .collect();
        EventLogPage {
            entries,
            head_sequence: inner.next.saturating_sub(1),
            truncated_before: inner.truncated_before,
        }
    }

    /// 保留边界内最新的 `limit` 条（升序返回）。
    pub fn latest(&self, limit: usize) -> Vec<T>
    where
        T: Clone,
    {
        let inner = self.inner.lock();
        let skip = inner.entries.len().saturating_sub(limit);
        inner
            .entries
            .iter()
            .skip(skip)
            .map(|(_, value)| value.clone())
            .collect()
    }
}

#[cfg(test)]
#[path = "event_log_test.rs"]
mod tests;
