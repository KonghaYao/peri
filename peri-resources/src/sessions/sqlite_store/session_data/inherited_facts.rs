//! inherited 冻结前缀的信封遍历、轻量摘要、全量布局与有界缓存。
//!
//! [`walk_payload_spans`] 是 summary 与 layout 的共同结构权威：两者对同一个 canonical
//! 信封得到相同的 payload 顺序与条数。summary 只保留首尾身份、条数与列字节数，不保留
//! 全部 payload；layout 才构造 payload 表与稳定的 `id -> 下标` 映射。
//!
//! 性能边界：全列扫描仍是派生索引缺失、损坏或列总字节数不符时的回退路径，O(列总字节)；
//! 索引命中时只读快照行与逐条区间，不做列扫描。索引是派生对象，事实仍以 canonical 信封
//! 为准；冷路径量化证据见 `session_data/inherited_perf_test.rs`。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;

use super::blob_reader::{BlobReadError, BlobReader};
use super::inherited_blob::{read_payload_id, Cursor, PayloadDecoding};

// 测试探针：线程内记录全列回退扫描次数，避免并行测试互相污染。
#[cfg(test)]
thread_local! {
    static FALLBACK_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_fallback_scan_probe() {
    FALLBACK_SCANS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn fallback_scan_count() -> usize {
    FALLBACK_SCANS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_fallback_scan() {
    FALLBACK_SCANS.with(|count| count.set(count.get() + 1));
}

/// 一条 payload 在列内的转义原文区间（不含两端引号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PayloadSpan {
    pub(super) raw_start: u64,
    pub(super) raw_len: u64,
}

/// 信封遍历结果：列总字节与 payload 条数。
struct PayloadWalk {
    total_bytes: u64,
    count: usize,
}

/// inherited 冻结前缀的轻量事实：首尾身份、条数与列字节数。
pub(super) struct InheritedSummary {
    pub(super) total_bytes: u64,
    pub(super) count: usize,
    pub(super) first_id: Option<MessageId>,
    pub(super) last_id: Option<MessageId>,
}

/// canonical 信封的布局：payload 顺序、原始区间与稳定身份。
///
/// `positions` 是 `id -> 首次出现的数组下标`；与线性扫描保持同一口径（重复身份取首个）。
pub(super) struct InheritedLayout {
    pub(super) total_bytes: u64,
    pub(super) payloads: Vec<LayoutPayload>,
    pub(super) positions: HashMap<MessageId, usize>,
}

pub(super) struct LayoutPayload {
    pub(super) span: PayloadSpan,
    pub(super) id: MessageId,
    /// 派生索引给出的逻辑长度；legacy 回填与全列扫描没有这一事实时为空，
    /// 由 [`InheritedLayout::logical_len`] 按需解码。
    pub(super) indexed_logical_len: Option<u64>,
    /// 逻辑长度与检查点；只在块读取时按需构建一次。
    pub(super) decoded: OnceLock<Arc<PayloadDecoding>>,
}

impl InheritedLayout {
    pub(super) fn len(&self) -> usize {
        self.payloads.len()
    }

    pub(super) fn first_id(&self) -> Option<MessageId> {
        self.payloads.first().map(|payload| payload.id)
    }

    pub(super) fn last_id(&self) -> Option<MessageId> {
        self.payloads.last().map(|payload| payload.id)
    }

    pub(super) fn position_of(&self, entry_id: MessageId) -> Option<usize> {
        self.positions.get(&entry_id).copied()
    }

    /// 数组下标处的稳定身份；越界即布局与请求不一致。
    pub(super) fn id_at(&self, index: usize) -> Option<MessageId> {
        self.payloads.get(index).map(|payload| payload.id)
    }

    pub(super) fn summary(&self) -> InheritedSummary {
        InheritedSummary {
            total_bytes: self.total_bytes,
            count: self.payloads.len(),
            first_id: self.first_id(),
            last_id: self.last_id(),
        }
    }
}

/// 逐个产出 payload span；结构校验与 summary/layout 的两个消费者完全共享。
fn walk_payload_spans(
    reader: &BlobReader,
    mut visit: impl FnMut(PayloadSpan) -> Result<(), BlobReadError>,
) -> Result<PayloadWalk, BlobReadError> {
    if reader.total() == 0 {
        return Err(BlobReadError::Unsupported);
    }
    let mut cursor = Cursor::new(reader);
    cursor.skip_ws()?;
    cursor.expect(b'{')?;
    cursor.skip_ws()?;
    cursor.expect_key(b"version")?;
    cursor.skip_ws()?;
    cursor.expect(b':')?;
    cursor.skip_ws()?;
    if cursor.take_number()? != Some(1) {
        return Err(BlobReadError::Unsupported);
    }
    cursor.skip_ws()?;
    cursor.expect(b',')?;
    cursor.skip_ws()?;
    cursor.expect_key(b"payloads")?;
    cursor.skip_ws()?;
    cursor.expect(b':')?;
    cursor.skip_ws()?;
    cursor.expect(b'[')?;

    let mut count = 0usize;
    cursor.skip_ws()?;
    if cursor.peek()? == Some(b']') {
        cursor.expect(b']')?;
    } else {
        loop {
            cursor.skip_ws()?;
            cursor.expect(b'"')?;
            let raw_start = cursor.position();
            cursor.skip_string_content()?;
            let raw_end = cursor.position();
            visit(PayloadSpan {
                raw_start,
                raw_len: raw_end.saturating_sub(raw_start),
            })?;
            count += 1;
            cursor.expect(b'"')?;
            cursor.skip_ws()?;
            match cursor.take()? {
                Some(b',') => continue,
                Some(b']') => break,
                _ => return Err(BlobReadError::Unsupported),
            }
        }
    }
    // 数组之后仍必须是合法的顶层对象尾部：截断/损坏不得被当成可用快照。
    cursor.verify_document_tail()?;
    Ok(PayloadWalk {
        total_bytes: reader.total(),
        count,
    })
}

/// 轻量摘要：只读取首尾 payload 的身份，不为中间条目构造布局。
pub(super) fn scan_summary(reader: &BlobReader) -> Result<InheritedSummary, BlobReadError> {
    #[cfg(test)]
    record_fallback_scan();
    let mut first = None;
    let mut last = None;
    let walk = walk_payload_spans(reader, |span| {
        if first.is_none() {
            first = Some(span);
        }
        last = Some(span);
        Ok(())
    })?;
    let first_id = match first {
        Some(span) => Some(read_payload_id(reader, span)?.ok_or(BlobReadError::Unsupported)?),
        None => None,
    };
    // 单条 payload 的 first 与 last 必须是同一个身份；避免重复读取造成的口径漂移。
    let last_id = if walk.count <= 1 {
        first_id
    } else {
        match last {
            Some(span) => Some(read_payload_id(reader, span)?.ok_or(BlobReadError::Unsupported)?),
            None => None,
        }
    };
    Ok(InheritedSummary {
        total_bytes: walk.total_bytes,
        count: walk.count,
        first_id,
        last_id,
    })
}

/// 完整布局：遍历一次 span，再为每条 payload 读取稳定身份并建立位置映射。
pub(super) fn scan_layout(reader: &BlobReader) -> Result<InheritedLayout, BlobReadError> {
    #[cfg(test)]
    record_fallback_scan();
    let mut spans = Vec::new();
    let walk = walk_payload_spans(reader, |span| {
        spans.push(span);
        Ok(())
    })?;
    debug_assert_eq!(spans.len(), walk.count);
    let mut payloads = Vec::with_capacity(spans.len());
    let mut positions = HashMap::with_capacity(spans.len());
    for span in spans {
        let Some(id) = read_payload_id(reader, span)? else {
            return Err(BlobReadError::Unsupported);
        };
        // 与线性 `position()` 一致：重复身份保留首次出现的下标。
        positions.entry(id).or_insert(payloads.len());
        payloads.push(LayoutPayload {
            span,
            id,
            indexed_logical_len: None,
            decoded: OnceLock::new(),
        });
    }
    Ok(InheritedLayout {
        total_bytes: walk.total_bytes,
        payloads,
        positions,
    })
}

/// 线程级缓存容量：按 LRU 淘汰，避免只插不淘汰导致长进程无界增长。
pub(super) const INHERITED_FACTS_CACHE_CAPACITY: usize = 16;

pub(super) enum CachedInheritedFacts {
    Unsupported { total_bytes: u64 },
    Summary(Arc<InheritedSummary>),
    Layout(Arc<InheritedLayout>),
}

pub(super) enum InheritedCacheLookup {
    Miss,
    Unsupported,
    Summary(Arc<InheritedSummary>),
    Layout(Arc<InheritedLayout>),
}

/// `(ThreadId, 列总字节数)` 命中的有界 LRU 缓存。总字节数变化即视为新快照。
pub(super) struct InheritedFactsCache {
    capacity: usize,
    entries: HashMap<ThreadId, CachedInheritedFacts>,
    order: VecDeque<ThreadId>,
}

impl InheritedFactsCache {
    pub(super) fn new() -> Self {
        Self::with_capacity(INHERITED_FACTS_CACHE_CAPACITY)
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// 查找并提升命中项；未命中（含总字节数不符）不改变顺序。
    pub(super) fn lookup(&mut self, id: &ThreadId, total_bytes: u64) -> InheritedCacheLookup {
        let lookup = match self.entries.get(id) {
            Some(CachedInheritedFacts::Unsupported {
                total_bytes: cached,
            }) if *cached == total_bytes => InheritedCacheLookup::Unsupported,
            Some(CachedInheritedFacts::Summary(summary)) if summary.total_bytes == total_bytes => {
                InheritedCacheLookup::Summary(Arc::clone(summary))
            }
            Some(CachedInheritedFacts::Layout(layout)) if layout.total_bytes == total_bytes => {
                InheritedCacheLookup::Layout(Arc::clone(layout))
            }
            _ => return InheritedCacheLookup::Miss,
        };
        self.touch(id);
        lookup
    }

    /// 写入并同步淘汰：替换已有条目时只提升顺序，不重复占位。
    pub(super) fn store(&mut self, id: &ThreadId, entry: CachedInheritedFacts) {
        if self.entries.insert(id.clone(), entry).is_some() {
            self.touch(id);
            return;
        }
        self.order.push_back(id.clone());
        while self.entries.len() > self.capacity {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&evicted);
        }
    }

    fn touch(&mut self, id: &ThreadId) {
        if let Some(position) = self.order.iter().position(|candidate| candidate == id) {
            self.order.remove(position);
        }
        self.order.push_back(id.clone());
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}
