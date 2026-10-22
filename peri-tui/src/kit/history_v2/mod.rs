//! H5a/H5b：History V2 的 TUI 单一状态、首屏准入与翻页 reducer。
//!
//! 本模块是 V2 历史在 TUI 的唯一状态模型：`session/load` 的 initial 页通过
//! [`initial::admit_load_response`] 一次性建立窗口身份（window/ticket/token/游标/
//! coverage）与交付边界（deliveryId/liveBoundary）。H5b 的 page/seek/body 合并扩展
//! 同一个 [`HistoryV2Window`]，不另建窗口状态。
//!
//! ## 请求与迟到丢弃
//!
//! [`paging`] 维护窗口边界 cursor、在途请求、导航代次、coverage/gap 与保留预算；
//! [`body`] 维护正文分块。服务端响应不回传 requestKey，调用方把请求时的
//! [`request::HistoryV2RequestContext`] 原样交回 `settle_*`；key/epoch/window/snapshot
//! 任一不匹配都整体丢弃，不写入新状态，也不自动重发旧请求。
//!
//! ## 开关
//!
//! [`history_v2_beta_enabled`] 默认 **false**（`history-v2` 本地 beta）。本地开关
//! 与连接协商结果必须同时为真才启用 V2（[`active`]）；否则旧 V1 / 标准路径逐字节
//! 保持不变。V2 协商成功时客户端不再声明 `peri.historyWindowV1`，服务端也不回放
//! V1 历史，因此 `load_session` 必须使用 initial 首屏而不是再发一次 open。
//!
//! ## 可执行与可阅读
//!
//! `executionReady=false` 只关闭 [`HistoryV2State::is_executable`]，不改变
//! [`HistoryV2State::is_ready`]：历史页仍然完整可读。二者是独立事实，不能把
//! 「执行未就绪」误投影成「历史失败」。

pub mod body;
pub mod consumer;
pub mod error;
pub mod initial;
pub mod live;
pub mod paging;
pub mod request;
mod validate;

pub(crate) use validate::{sub_item_descriptor, sub_item_id};

use std::collections::VecDeque;

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_META_KEY, HistoryCoverageInterval, HistoryEntry, HistoryLiveBoundary,
    HistoryProgress, HistoryServerCapability,
};
use serde_json::{Map, Value};

use crate::kit::atoms;
use paging::{HistoryV2Gap, HistoryV2Paging, HistoryV2Segment};

#[cfg(test)]
#[path = "initial_test.rs"]
mod initial_test;

#[cfg(test)]
#[path = "paging_test.rs"]
mod paging_test;

#[cfg(test)]
#[path = "body_test.rs"]
mod body_test;

/// TUI 本地 V2 beta 键；未配置 = 关闭。
pub const HISTORY_V2_BETA: &str = peri_config::betas::HISTORY_V2;

/// V2 状态机的会话级阶段。首屏只产生 `Idle → Ready | Failed`；H5b 的
/// live 连续性与窗口刷新阶段在此扩展。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryV2Status {
    #[default]
    Idle,
    /// initial 已原子接纳；窗口与交付边界可用。
    Ready,
    /// initial 被拒绝；本会话不得进入可执行态。
    Failed,
}

/// initial 首屏建立的窗口身份、页面内容与 H5b 页面状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Window {
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    pub history_revision: DecimalU64,
    pub committed_tail: Option<DecimalU64>,
    pub projection_version: u32,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub items: Vec<HistoryEntry>,
    /// 已物化的 coverage 区间（按逻辑顺序、尽量合并）。
    pub coverage: Vec<HistoryCoverageInterval>,
    /// `emptySnapshot`：存储逻辑范围确实为空。
    pub empty_snapshot: bool,
    /// 保留窗口内显式未物化的空洞。
    pub gaps: Vec<HistoryV2Gap>,
    /// 保留页段；淘汰与边界游标以整页为单位。
    pub segments: VecDeque<HistoryV2Segment>,
    pub retained_items: usize,
    pub retained_bytes: usize,
    pub paging: HistoryV2Paging,
    /// 最近一页的读计划结果；用于展示扫描预算/进度。
    pub progress: HistoryProgress,
}

/// 首屏失败事实；只保存可展示的类型化原因，不携带 wire 原文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2Failure {
    pub reason: initial::InitialRejection,
    pub detail: String,
}

/// 唯一的 V2 历史状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryV2State {
    pub status: HistoryV2Status,
    pub session_id: Option<String>,
    /// 服务端执行事实；与历史可读性独立。
    pub execution_ready: bool,
    pub runtime_generation: Option<String>,
    pub delivery_id: Option<String>,
    pub live_boundary: Option<HistoryLiveBoundary>,
    pub window: Option<HistoryV2Window>,
    pub failure: Option<HistoryV2Failure>,
}

impl HistoryV2State {
    /// initial 已接纳，历史可读（即使 `execution_ready == false`）。
    pub fn is_ready(&self) -> bool {
        self.status == HistoryV2Status::Ready
    }

    /// 可执行 = 首屏接纳且服务端声明执行就绪。
    pub fn is_executable(&self) -> bool {
        self.is_ready() && self.execution_ready
    }

    /// H5b 的页合并入口读取窗口；未就绪时为 `None`。
    pub fn window(&self) -> Option<&HistoryV2Window> {
        self.window.as_ref()
    }

    /// H5b 的 reducer 写入口；调用方仍通过本状态整体发布，不建第二份窗口。
    pub fn window_mut(&mut self) -> Option<&mut HistoryV2Window> {
        self.window.as_mut()
    }
}

/// 读取本地 beta 配置；与 V1 的「未配置视为开启」不同，V2 默认关闭。
pub fn history_v2_beta_enabled(config: &crate::config::PeriConfig) -> bool {
    config.config.betas.get(HISTORY_V2_BETA).unwrap_or(false)
}

/// 从合并快照的 beta 投影读取同一语义：只有显式 `true` 才开启。
pub fn history_v2_beta_enabled_from_flags(flags: &peri_config::betas::BetaFlags) -> bool {
    flags
        .value(HISTORY_V2_BETA)
        .is_some_and(|value| value.enabled)
}

/// 启动时写入本地开关（只写一次；无热重载）。
pub fn apply_history_v2_beta(enabled: bool) {
    atoms::HISTORY_V2.set(enabled);
}

/// 唯一写入点：initial 接纳成功。
pub fn publish_initial(state: HistoryV2State) {
    debug_assert_eq!(state.status, HistoryV2Status::Ready);
    if let Some(key) = state
        .window
        .as_ref()
        .and_then(|window| window.paging.last_request_key)
    {
        consumer::seed_request_key(key);
    }
    atoms::HISTORY_V2_STATE.set(state.clone());
    consumer::notify_view(&state);
    // F2b：首屏先就绪，再按 eventSeq 重放 initial 前暂存的 live 更新。视图事件与
    // 重放事件都经 LOCAL_EVENT_TX，同一发送者的顺序保证「先首屏、后尾部」。
    let replayed = crate::kit::acp_notifier::replay_pending_initial(&state);
    crate::kit::acp_notifier::dispatch_local_events(replayed);
}

/// 唯一失败写入点：保留失败事实，状态永不进入 Ready。
pub fn publish_failure(session_id: &str, failure: HistoryV2Failure) {
    crate::kit::history_v2::live::clear_pending(session_id);
    let state = HistoryV2State {
        status: HistoryV2Status::Failed,
        session_id: Some(session_id.to_owned()),
        failure: Some(failure),
        ..HistoryV2State::default()
    };
    atoms::HISTORY_V2_STATE.set(state.clone());
    consumer::notify_view(&state);
}

/// 会话边界/切换连接时清空；在途响应随后按 sessionId 自判丢弃。
pub fn reset_state() {
    live::clear_all_pending();
    atoms::HISTORY_V2_STATE.set(HistoryV2State::default());
}

/// 只读取当前状态快照。
pub fn snapshot() -> HistoryV2State {
    atoms::HISTORY_V2_STATE.state().read().clone()
}

/// 解析 initialize 响应中的服务端能力；只有明确回显 V2/windowed 才算协商成功。
pub(crate) fn negotiate_from_initialize_response(
    result: &Value,
) -> Result<Option<HistoryServerCapability>, String> {
    let meta = result
        .pointer("/agentCapabilities/_meta")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    if meta.get(HISTORY_META_KEY).is_none() {
        return Ok(None);
    }
    HistoryServerCapability::parse_from_meta(&meta)
        .map_err(|error| format!("history v2 capability rejected: {error}"))
}
