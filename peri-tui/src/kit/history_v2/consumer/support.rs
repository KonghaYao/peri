//! 窗口身份、详情正文请求与会话边界复位。

use peri_acp_types::history_v2::DecimalU64;
use std::sync::atomic::Ordering;

use crate::kit::atoms;

use super::super::outbound::{self, Submit};
use super::super::HistoryV2State;
use super::CONSUMER_EPOCH;

/// 一个已交付窗口的关闭身份。关闭只依赖服务端事实，不读回当前状态，避免在持有
/// 状态锁时再次进入入队临界区。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryV2WindowIdentity {
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
}

impl HistoryV2WindowIdentity {
    pub(crate) fn from_state(state: &HistoryV2State) -> Option<Self> {
        state.window.as_ref().map(Self::from_window)
    }

    pub(super) fn from_window(window: &super::super::HistoryV2Window) -> Self {
        Self {
            session_id: window.session_id.clone(),
            window_id: window.window_id.clone(),
            snapshot_token: window.snapshot_token.clone(),
            read_ticket: window.read_ticket.clone(),
        }
    }
}

/// H5c：详情面板的正文请求。详情是窗口内容的派生阅读器：按 §11.4 使用独立的
/// 详情预算和随机 offset 语义，不写窗口页/正文账目，也不参与窗口代次结算；
/// 响应由 `HistoryV2BodyBinding` 的 window/snapshot/bodyRef 校验。requestKey 与
/// 页/seek/open/initial 共用同一分配器，并且与入队在同一临界区内完成。
pub(crate) fn submit_body(
    binding: &crate::kit::history_detail_binding::HistoryV2BodyBinding,
    offset: u64,
    max_bytes: u32,
) -> Result<Submit<super::super::request::HistoryV2BodyRequest>, outbound::SubmitError> {
    outbound::submit_body(|request_key| super::super::request::HistoryV2BodyRequest {
        context: super::super::request::HistoryV2RequestContext {
            request_key,
            session_id: binding.session_id.clone(),
            window_id: binding.window_id.clone(),
            snapshot_token: binding.snapshot_token.clone(),
            read_ticket: binding.read_ticket.clone(),
            view_epoch: 0,
        },
        body_key: String::new(),
        body_ref: binding.body_ref.clone(),
        offset: DecimalU64::new(offset),
        max_bytes,
    })
}

/// 桥接 reset 只清理视图投影；在途认领由生命周期同步动作（new/load/delete）
/// 清理，避免桥接任务的异步观察晚于新会话的 Open 入队。
pub(crate) fn reset_view_projection() {
    atoms::HISTORY_V2_PREFIX_LEN.set(0);
    atoms::HISTORY_V2_VIEW_EPOCH.set(0);
}

/// 会话边界复位：作废在途 open 并重置视图投影。
pub(crate) fn reset_consumer_state() {
    *super::open_claim_slot() = None;
    CONSUMER_EPOCH.fetch_add(1, Ordering::Relaxed);
    reset_view_projection();
}
