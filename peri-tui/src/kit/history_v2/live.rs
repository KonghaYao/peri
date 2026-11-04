//! H5b/F2b：实时投递边界（设计 §6.3、§9.4、§9.5、§12.1）。
//!
//! initial 的 `liveBoundary` 之前属于首屏覆盖范围；只有 `afterSeq + 1` 起的连续事件
//! 才能写入。发现缺口立即进入 detached 并记录缺口，本阶段不做自动恢复。
//!
//! 服务端在 transcript 更新顶层 `_meta["peri.delivery"]` 携带
//! `{streamId, deliveryId, eventSeq}`。客户端在渲染前先做接纳判定；没有该元数据
//! （V1 路径）时完全保持原有行为。initial 发布前到达的更新先进入每会话有界暂存，
//! 发布后按 seq 重放；失败/切换/关闭时丢弃，不跨会话泄漏。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use peri_acp_types::history_v2::{DecimalU64, HistoryLiveBoundary};
use serde_json::Value;

use super::HistoryV2Window;
use crate::kit::atoms;

/// 首屏暂存上限：条数与序列化字节数取先到者（设计 §6.4、§10.5）。
pub(crate) const MAX_PENDING_UPDATES: usize = 64;
pub(crate) const MAX_PENDING_BYTES: usize = 256 * 1024;

/// 实时投递状态；缺口只记录，不假装连续。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryV2LiveState {
    pub stream_id: Option<String>,
    pub last_applied_seq: Option<DecimalU64>,
    pub gap: Option<HistoryV2LiveGap>,
    pub detached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2LiveGap {
    pub expected: DecimalU64,
    pub received: DecimalU64,
}

impl HistoryV2LiveState {
    pub fn from_boundary(boundary: &HistoryLiveBoundary) -> Self {
        Self {
            stream_id: Some(boundary.stream_id.clone()),
            last_applied_seq: Some(boundary.after_seq),
            gap: None,
            detached: false,
        }
    }
}

/// 实时事件接纳结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2LiveAdmission {
    Accepted {
        seq: DecimalU64,
    },
    /// `seq < liveBoundary.afterSeq`：属于首屏已覆盖范围。
    BeforeBoundary,
    Duplicate,
    /// 发现缺口；进入 detached，等待重新 initial/open。
    GapRecorded {
        expected: DecimalU64,
        received: DecimalU64,
    },
    Detached,
    WrongStream,
    NotReady,
}

impl HistoryV2Window {
    /// 只有 `liveBoundary` 之后的连续事件才被接纳；缺口显式记录。
    pub fn accept_live_event(
        &mut self,
        stream_id: &str,
        seq: DecimalU64,
    ) -> HistoryV2LiveAdmission {
        let Some(expected_stream) = self.paging.live.stream_id.as_deref() else {
            return HistoryV2LiveAdmission::NotReady;
        };
        if expected_stream != stream_id {
            return HistoryV2LiveAdmission::WrongStream;
        }
        if self.paging.live.detached {
            return HistoryV2LiveAdmission::Detached;
        }
        let Some(last) = self.paging.live.last_applied_seq else {
            return HistoryV2LiveAdmission::NotReady;
        };
        if seq < last {
            return HistoryV2LiveAdmission::BeforeBoundary;
        }
        if seq == last {
            return HistoryV2LiveAdmission::Duplicate;
        }
        let expected = match last.get().checked_add(1) {
            Some(value) => DecimalU64::new(value),
            None => return HistoryV2LiveAdmission::NotReady,
        };
        if seq == expected {
            self.paging.live.last_applied_seq = Some(seq);
            return HistoryV2LiveAdmission::Accepted { seq };
        }
        self.paging.live.gap = Some(HistoryV2LiveGap {
            expected,
            received: seq,
        });
        self.paging.live.detached = true;
        HistoryV2LiveAdmission::GapRecorded {
            expected,
            received: seq,
        }
    }
}

/// 一条 transcript 更新上的投递身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryMeta {
    pub(crate) stream_id: String,
    pub(crate) delivery_id: String,
    pub(crate) seq: DecimalU64,
}

impl DeliveryMeta {
    /// 解析顶层 `_meta["peri.delivery"]`；形状错误一律视为没有元数据。
    ///
    /// `eventSeq` 必须是十进制字符串（服务端契约），不接受的数字/浮点形状回退到
    /// V1 渲染路径，而不是 panic 或猜测序号。
    pub(crate) fn parse(params: &Value) -> Option<Self> {
        let delivery = params.get("_meta")?.get("peri.delivery")?;
        let stream_id = delivery.get("streamId")?.as_str()?;
        let delivery_id = delivery.get("deliveryId")?.as_str()?;
        let seq = delivery.get("eventSeq")?.as_str()?.parse::<u64>().ok()?;
        if stream_id.is_empty() || delivery_id.is_empty() {
            return None;
        }
        Some(Self {
            stream_id: stream_id.to_owned(),
            delivery_id: delivery_id.to_owned(),
            seq: DecimalU64::new(seq),
        })
    }
}

/// 只有这五类更新进入实时交付序列（与服务端 `is_transcript_update` 对齐）。
pub(crate) fn is_transcript_update_tag(tag: Option<&str>) -> bool {
    matches!(
        tag,
        Some(
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "user_message_chunk"
                | "tool_call"
                | "tool_call_update"
        )
    )
}

/// 渲染门：`Skip` 表示本条更新已被接纳逻辑消费，不得再追加到 bridge/transcript。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveGate {
    Render,
    Skip,
}

/// 通知入口的接纳门：解析投递元数据 → 暂存或按序接纳。
///
/// 没有元数据（V1/普通连接）直接放行；非 transcript 更新不参与序号，也直接放行。
pub(crate) fn gate_session_update(session_id: &str, tag: Option<&str>, params: &Value) -> LiveGate {
    if !is_transcript_update_tag(tag) {
        return LiveGate::Render;
    }
    let Some(meta) = DeliveryMeta::parse(params) else {
        return LiveGate::Render;
    };
    if push_pending(session_id, &meta, params) {
        tracing::debug!(
            session_id = %session_id,
            stream_id = %meta.stream_id,
            seq = meta.seq.get(),
            "history v2: live update buffered until initial is published"
        );
        return LiveGate::Skip;
    }
    match admit_live_delivery(session_id, &meta) {
        HistoryV2LiveAdmission::Accepted { .. } => LiveGate::Render,
        HistoryV2LiveAdmission::NotReady => {
            tracing::warn!(
                session_id = %session_id,
                stream_id = %meta.stream_id,
                seq = meta.seq.get(),
                "history v2: live update dropped (history window not ready)"
            );
            LiveGate::Skip
        }
        admission => {
            tracing::debug!(
                session_id = %session_id,
                stream_id = %meta.stream_id,
                delivery_id = %meta.delivery_id,
                seq = meta.seq.get(),
                ?admission,
                "history v2: live update not rendered"
            );
            LiveGate::Skip
        }
    }
}

/// 在唯一窗口状态上执行接纳；缺口记录后发布一次视图让 warning note 可见。
pub(crate) fn admit_live_delivery(session_id: &str, meta: &DeliveryMeta) -> HistoryV2LiveAdmission {
    let handle = atoms::HISTORY_V2_STATE.state();
    let mut state = handle.write();
    if state.session_id.as_deref() != Some(session_id) || !state.is_ready() {
        return HistoryV2LiveAdmission::NotReady;
    }
    let Some(window) = state.window_mut() else {
        return HistoryV2LiveAdmission::NotReady;
    };
    let admission = window.accept_live_event(&meta.stream_id, meta.seq);
    let notify =
        matches!(admission, HistoryV2LiveAdmission::GapRecorded { .. }).then(|| state.clone());
    drop(state);
    if let Some(snapshot) = notify {
        super::consumer::notify_view(&snapshot);
    }
    admission
}

struct PendingUpdate {
    meta: DeliveryMeta,
    params: Value,
    bytes: usize,
}

#[derive(Default)]
struct PendingSession {
    updates: VecDeque<PendingUpdate>,
    bytes: usize,
    /// 溢出丢弃过最旧更新；重放时由 eventSeq 缺口显式进入 detached。
    dropped: bool,
}

#[derive(Default)]
struct PendingStore {
    sessions: HashMap<String, PendingSession>,
}

static PENDING: Mutex<Option<PendingStore>> = Mutex::new(None);

fn pending_lock() -> MutexGuard<'static, Option<PendingStore>> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 发 load 请求前登记：该会话进入「initial 未发布」阶段。
pub(crate) fn begin_pending(session_id: &str) {
    let mut guard = pending_lock();
    guard
        .get_or_insert_with(PendingStore::default)
        .sessions
        .insert(session_id.to_owned(), PendingSession::default());
}

/// 该会话当前是否仍在等待 initial 发布。
#[cfg(test)]
pub(crate) fn is_pending(session_id: &str) -> bool {
    pending_lock()
        .as_ref()
        .is_some_and(|store| store.sessions.contains_key(session_id))
}

fn push_pending(session_id: &str, meta: &DeliveryMeta, params: &Value) -> bool {
    let mut guard = pending_lock();
    let Some(store) = guard.as_mut() else {
        return false;
    };
    let Some(pending) = store.sessions.get_mut(session_id) else {
        return false;
    };
    // 同一 stream + seq 的重复通知只保留一份；重放时状态机也会再拒绝一次。
    if pending
        .updates
        .iter()
        .any(|update| update.meta.stream_id == meta.stream_id && update.meta.seq == meta.seq)
    {
        return true;
    }
    let bytes = serde_json::to_vec(params)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    pending.updates.push_back(PendingUpdate {
        meta: meta.clone(),
        params: params.clone(),
        bytes,
    });
    pending.bytes = pending.bytes.saturating_add(bytes);
    while pending.updates.len() > MAX_PENDING_UPDATES || pending.bytes > MAX_PENDING_BYTES {
        let Some(dropped) = pending.updates.pop_front() else {
            break;
        };
        pending.bytes = pending.bytes.saturating_sub(dropped.bytes);
        pending.dropped = true;
    }
    true
}

/// initial 发布后取走该会话的暂存（按 seq 稳定排序）。
pub(crate) fn take_pending(session_id: &str) -> Vec<(DeliveryMeta, Value)> {
    let mut guard = pending_lock();
    let Some(store) = guard.as_mut() else {
        return Vec::new();
    };
    let Some(mut pending) = store.sessions.remove(session_id) else {
        return Vec::new();
    };
    if pending.dropped {
        tracing::warn!(
            session_id = %session_id,
            "history v2: initial pending buffer overflowed; replay starts at a gap"
        );
    }
    pending
        .updates
        .make_contiguous()
        .sort_by_key(|update| update.meta.seq.get());
    pending
        .updates
        .into_iter()
        .map(|update| (update.meta, update.params))
        .collect()
}

/// 丢弃该会话的暂存（load 失败/切换/关闭）。
pub(crate) fn clear_pending(session_id: &str) {
    if let Some(store) = pending_lock().as_mut() {
        store.sessions.remove(session_id);
    }
}

/// 进程级兜底：会话边界重置时清空所有暂存，避免跨会话泄漏。
pub(crate) fn clear_all_pending() {
    if let Some(store) = pending_lock().as_mut() {
        store.sessions.clear();
    }
}

#[cfg(test)]
#[path = "live_test.rs"]
mod tests;
