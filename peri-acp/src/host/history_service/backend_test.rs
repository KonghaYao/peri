//! History V2 服务测试替身：可注入结果、可阻塞、可观测调用的 fake backend。
//!
//! 只实现端口，不模拟 SQLite；所有"存储事实"由测试显式给出。

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::Notify;

use peri_acp_types::history_v2::{
    DecimalU64, HistoryAnchor, HistoryBackendError, HistoryBodyDescriptor, HistoryBodySlice,
    HistoryCommittedTailRef, HistoryCoverage, HistoryDirection, HistoryDurability,
    HistoryDurabilityState, HistoryEncoding, HistoryEntry, HistoryEntryAnchor, HistoryLimits,
    HistoryOpaqueSubItem, HistoryOpenedSnapshot, HistoryOrigin, HistoryPageData, HistoryProgress,
    HistoryProgressReason, HistoryRepresentation, HistorySnapshotRef, HistorySource,
    HistorySubItem, HistoryTextSubItem, HistoryV2Backend,
};

use super::state::HistoryClock;

/// 手动推进的单调时钟。
pub(crate) struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    pub(crate) fn new() -> Self {
        Self {
            now: Mutex::new(Duration::ZERO),
        }
    }

    pub(crate) fn advance(&self, delta: Duration) {
        let mut now = self.now.lock();
        *now += delta;
    }
}

impl HistoryClock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OpenCall {
    pub(crate) session_id: String,
    pub(crate) anchor: HistoryAnchor,
    pub(crate) before: u32,
    pub(crate) after: u32,
    pub(crate) target_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PageCall {
    pub(crate) raw_cursor: String,
    pub(crate) direction: HistoryDirection,
    pub(crate) limit: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct BodyCall {
    pub(crate) body_ref: String,
    pub(crate) offset: u64,
    pub(crate) max_bytes: u64,
}

pub(crate) struct FakeBackend {
    open_result: Mutex<Result<HistoryOpenedSnapshot, HistoryBackendError>>,
    page_result: Mutex<Result<HistoryPageData, HistoryBackendError>>,
    seek_result: Mutex<Result<HistoryPageData, HistoryBackendError>>,
    body_result: Mutex<Result<HistoryBodySlice, HistoryBackendError>>,
    pub(crate) open_calls: Mutex<Vec<OpenCall>>,
    pub(crate) page_calls: Mutex<Vec<PageCall>>,
    pub(crate) body_calls: Mutex<Vec<BodyCall>>,
    open_gates: Mutex<VecDeque<Arc<Notify>>>,
    page_gates: Mutex<VecDeque<Arc<Notify>>>,
    body_gates: Mutex<VecDeque<Arc<Notify>>>,
    pub(crate) open_entered: Arc<Notify>,
    pub(crate) page_entered: Arc<Notify>,
    pub(crate) body_entered: Arc<Notify>,
}

impl FakeBackend {
    pub(crate) fn new(open: HistoryOpenedSnapshot) -> Self {
        Self {
            open_result: Mutex::new(Ok(open)),
            page_result: Mutex::new(Ok(page_data(0, None, None))),
            seek_result: Mutex::new(Ok(page_data(0, None, None))),
            body_result: Mutex::new(Ok(body_slice())),
            open_calls: Mutex::new(Vec::new()),
            page_calls: Mutex::new(Vec::new()),
            body_calls: Mutex::new(Vec::new()),
            open_gates: Mutex::new(VecDeque::new()),
            page_gates: Mutex::new(VecDeque::new()),
            body_gates: Mutex::new(VecDeque::new()),
            open_entered: Arc::new(Notify::new()),
            page_entered: Arc::new(Notify::new()),
            body_entered: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn set_open_result(
        &self,
        result: Result<HistoryOpenedSnapshot, HistoryBackendError>,
    ) {
        *self.open_result.lock() = result;
    }

    pub(crate) fn set_page_result(&self, result: Result<HistoryPageData, HistoryBackendError>) {
        *self.page_result.lock() = result;
    }

    pub(crate) fn set_seek_result(&self, result: Result<HistoryPageData, HistoryBackendError>) {
        *self.seek_result.lock() = result;
    }

    pub(crate) fn set_body_result(&self, result: Result<HistoryBodySlice, HistoryBackendError>) {
        *self.body_result.lock() = result;
    }

    /// 让下一次 open 阻塞；返回的 Notify 用于释放。
    pub(crate) fn gate_next_open(&self) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        self.open_gates.lock().push_back(gate.clone());
        gate
    }

    /// 让下一次 page 阻塞；返回的 Notify 用于释放。
    pub(crate) fn gate_next_page(&self) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        self.page_gates.lock().push_back(gate.clone());
        gate
    }

    /// 让下一次 body 阻塞；返回的 Notify 用于释放。
    pub(crate) fn gate_next_body(&self) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        self.body_gates.lock().push_back(gate.clone());
        gate
    }

    async fn wait(gate: Option<Arc<Notify>>) {
        if let Some(gate) = gate {
            gate.notified().await;
        }
    }
}

#[async_trait]
impl HistoryV2Backend for FakeBackend {
    async fn open(
        &self,
        session_id: &str,
        anchor: &HistoryAnchor,
        before: u32,
        after: u32,
        target_bytes: u64,
    ) -> Result<HistoryOpenedSnapshot, HistoryBackendError> {
        self.open_calls.lock().push(OpenCall {
            session_id: session_id.to_owned(),
            anchor: anchor.clone(),
            before,
            after,
            target_bytes,
        });
        let gate = self.open_gates.lock().pop_front();
        if gate.is_some() {
            self.open_entered.notify_one();
        }
        Self::wait(gate).await;
        self.open_result.lock().clone()
    }

    async fn page(
        &self,
        _snapshot: &HistorySnapshotRef,
        raw_cursor: &str,
        direction: HistoryDirection,
        limit: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        self.page_calls.lock().push(PageCall {
            raw_cursor: raw_cursor.to_owned(),
            direction,
            limit,
        });
        let gate = self.page_gates.lock().pop_front();
        if gate.is_some() {
            self.page_entered.notify_one();
        }
        Self::wait(gate).await;
        self.page_result.lock().clone()
    }

    async fn seek(
        &self,
        _snapshot: &HistorySnapshotRef,
        _anchor: &HistoryEntryAnchor,
        _before: u32,
        _after: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        self.seek_result.lock().clone()
    }

    async fn body(
        &self,
        _snapshot: &HistorySnapshotRef,
        body_ref: &str,
        offset: u64,
        max_bytes: u64,
    ) -> Result<HistoryBodySlice, HistoryBackendError> {
        self.body_calls.lock().push(BodyCall {
            body_ref: body_ref.to_owned(),
            offset,
            max_bytes,
        });
        let gate = self.body_gates.lock().pop_front();
        if gate.is_some() {
            self.body_entered.notify_one();
        }
        Self::wait(gate).await;
        self.body_result.lock().clone()
    }
}

pub(crate) fn limits() -> HistoryLimits {
    HistoryLimits::default()
}

pub(crate) fn tail_ref() -> HistoryCommittedTailRef {
    HistoryCommittedTailRef {
        entry_id: "entry-42".to_owned(),
        position: 42,
    }
}

pub(crate) fn snap(revision: u64) -> HistoryOpenedSnapshot {
    HistoryOpenedSnapshot {
        history_revision: DecimalU64::new(revision),
        committed_tail: Some(tail_ref()),
        projection_version: 3,
        page: page_data(revision, Some("b0"), Some("a0")),
    }
}

pub(crate) fn page_data(
    revision: u64,
    before: Option<&str>,
    after: Option<&str>,
) -> HistoryPageData {
    HistoryPageData {
        entries: Vec::new(),
        coverage: HistoryCoverage::EmptySnapshot,
        progress: HistoryProgress {
            reason: HistoryProgressReason::Complete,
            scanned_entries: 0,
            consumed_entries: 0,
        },
        before_cursor: before.map(str::to_owned),
        after_cursor: after.map(str::to_owned),
        committed_tail: Some(tail_ref()),
        history_revision: DecimalU64::new(revision),
        projection_version: 3,
    }
}

pub(crate) fn big_page(bytes_per_entry: usize, entries: usize) -> HistoryPageData {
    let mut page = page_data(0, Some("b0"), Some("a0"));
    page.entries = (0..entries)
        .map(|index| HistoryEntry {
            entry_id: format!("e{index}"),
            source: HistorySource::Own,
            content_version: "v1".to_owned(),
            durability: HistoryDurability {
                state: HistoryDurabilityState::Sealed,
                origin: HistoryOrigin::Imported,
                final_item_seq: None,
            },
            order: peri_acp_types::history_v2::HistoryOrder {
                region: "own".to_owned(),
                ordinal: DecimalU64::new(index as u64),
            },
            sub_items: vec![HistorySubItem::Text(HistoryTextSubItem {
                sub_item_id: format!("s{index}"),
                role: "assistant".to_owned(),
                text: Some("x".repeat(bytes_per_entry)),
                body: None,
            })],
        })
        .collect();
    page
}

pub(crate) fn body_slice() -> HistoryBodySlice {
    HistoryBodySlice {
        body_ref: "body-1".to_owned(),
        content_version: "cv-1".to_owned(),
        representation: HistoryRepresentation::DisplayText,
        encoding: HistoryEncoding::Base64,
        media_type: "text/plain".to_owned(),
        offset: DecimalU64::ZERO,
        data: "aGVsbG8=".to_owned(),
        next_offset: None,
        complete: true,
        total_bytes: DecimalU64::new(5),
    }
}

/// 携带一个 opaque 子项的首屏：initial/open 会为该 `bodyRef` 签发读取授权。
pub(crate) fn opaque_page() -> HistoryPageData {
    let mut page = page_data(0, Some("b0"), Some("a0"));
    page.entries = vec![HistoryEntry {
        entry_id: "e0".to_owned(),
        source: HistorySource::Own,
        content_version: "v1".to_owned(),
        durability: HistoryDurability {
            state: HistoryDurabilityState::Sealed,
            origin: HistoryOrigin::Imported,
            final_item_seq: None,
        },
        order: peri_acp_types::history_v2::HistoryOrder {
            region: "own".to_owned(),
            ordinal: DecimalU64::ZERO,
        },
        sub_items: vec![HistorySubItem::Opaque(HistoryOpaqueSubItem {
            sub_item_id: "s0".to_owned(),
            body: HistoryBodyDescriptor {
                body_ref: "body-1".to_owned(),
                representation: HistoryRepresentation::DisplayText,
                media_type: "text/plain".to_owned(),
                total_bytes: DecimalU64::new(5),
                chunk_bytes: 65536,
            },
        })],
    }];
    page
}

pub(crate) fn open_params(key: u64, session: &str, before: u32, after: u32) -> Value {
    json!({
        "version": 2,
        "action": "open",
        "sessionId": session,
        "requestKey": key.to_string(),
        "anchor": { "kind": "latest" },
        "before": before,
        "after": after,
        "targetBytes": 524288
    })
}

pub(crate) fn open_entry_params(key: u64, session: &str, entry_id: &str) -> Value {
    json!({
        "version": 2,
        "action": "open",
        "sessionId": session,
        "requestKey": key.to_string(),
        "anchor": {
            "kind": "entry",
            "entryId": entry_id,
            "source": { "kind": "own" }
        },
        "before": 25,
        "after": 24,
        "targetBytes": 524288
    })
}

#[allow(clippy::too_many_arguments)] // 测试 helper 逐项对应 wire 字段。
pub(crate) fn page_params(
    key: u64,
    session: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
    cursor: Option<&str>,
    direction: &str,
    limit: u32,
) -> Value {
    let mut value = json!({
        "version": 2,
        "action": "page",
        "sessionId": session,
        "requestKey": key.to_string(),
        "windowId": window_id,
        "snapshotToken": snapshot_token,
        "readTicket": read_ticket,
        "direction": direction,
        "limit": limit
    });
    if let Some(cursor) = cursor {
        value["cursor"] = json!(cursor);
    }
    value
}

pub(crate) fn seek_params(
    key: u64,
    session: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
    entry_id: &str,
) -> Value {
    json!({
        "version": 2,
        "action": "seek",
        "sessionId": session,
        "requestKey": key.to_string(),
        "windowId": window_id,
        "snapshotToken": snapshot_token,
        "readTicket": read_ticket,
        "anchor": {
            "kind": "entry",
            "entryId": entry_id,
            "source": { "kind": "own" }
        },
        "before": 25,
        "after": 24
    })
}

pub(crate) fn body_params(
    key: u64,
    session: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
) -> Value {
    json!({
        "version": 2,
        "action": "body",
        "sessionId": session,
        "requestKey": key.to_string(),
        "windowId": window_id,
        "snapshotToken": snapshot_token,
        "readTicket": read_ticket,
        "bodyRef": "body-1",
        "offset": "0",
        "maxBytes": 65536
    })
}

pub(crate) fn close_params(
    key: u64,
    session: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
) -> Value {
    json!({
        "version": 2,
        "action": "close",
        "sessionId": session,
        "requestKey": key.to_string(),
        "windowId": window_id,
        "snapshotToken": snapshot_token,
        "readTicket": read_ticket
    })
}
