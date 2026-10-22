//! History V2 initial 首屏准入（设计 §6.2、§6.4）。
//!
//! 只接受 `_meta["peri.history"]` 的 initial 结果。V2 连接不得混入 V1 load 的
//! 顶层 `snapshotToken` / `nextCursor` 或 `_meta.periLoadGeneration`——出现即说明
//! 服务端仍在交付旧协议，客户端必须显式失败而不是挑选一边使用。

use std::collections::VecDeque;
use std::fmt;

use peri_acp_types::history_v2::{
    HISTORY_V2_VERSION, HistoryCoverage, HistoryInitialResult, HistoryLimits, HistoryWindowPage,
};
use serde_json::Value;

use super::live::HistoryV2LiveState;
use super::paging::{HistoryV2Home, HistoryV2Paging, HistoryV2PagingStatus, HistoryV2Retention};
use super::validate::{PageValidationError, validate_window_page};
use super::{HistoryV2Failure, HistoryV2State, HistoryV2Status, HistoryV2Window};
use crate::kit::history_v2::paging::HistoryV2Segment;

/// initial 被拒绝的类型化原因；稳定可断言，不随文案变化。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialRejection {
    /// V2 连接缺少 `_meta["peri.history"]`。
    Missing,
    /// 同时出现 V1 load 字段。
    MixedFields,
    /// 结构/身份/预算校验失败。
    Invalid,
    /// 页面属于另一会话。
    SessionMismatch,
}

impl fmt::Display for InitialRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Missing => "history_initial_required",
            Self::MixedFields => "history_initial_mixed_fields",
            Self::Invalid => "history_initial_invalid",
            Self::SessionMismatch => "history_initial_session_mismatch",
        };
        formatter.write_str(label)
    }
}

/// 准入失败：原因 + 不含 wire 原文的定位描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialAdmissionError {
    pub reason: InitialRejection,
    pub detail: String,
}

impl InitialAdmissionError {
    fn new(reason: InitialRejection, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    pub fn into_failure(self) -> HistoryV2Failure {
        HistoryV2Failure {
            reason: self.reason,
            detail: self.detail,
        }
    }
}

impl fmt::Display for InitialAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

/// 构造 `session/load` 的 initial 请求体（不含 `_meta` 外壳）。
pub fn initial_request_value(
    limits: &HistoryLimits,
    request_key: peri_acp_types::history_v2::DecimalU64,
) -> Value {
    serde_json::json!({
        "version": HISTORY_V2_VERSION,
        "action": "initial",
        "requestKey": request_key,
        "limit": limits.default_items,
        "targetBytes": limits.target_page_bytes,
    })
}

/// 从完整 load 响应原子接纳首屏。
///
/// 成功返回 `Ready` 状态；任何校验失败返回类型化错误，调用方必须发布失败状态且
/// 不提交 Stable。`executionReady=false` 不是失败：它只让
/// [`HistoryV2State::is_executable`] 为 false。
pub fn admit_load_response(
    response: &Value,
    expected_session: &str,
    limits: &HistoryLimits,
) -> Result<HistoryV2State, InitialAdmissionError> {
    let meta = response
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            InitialAdmissionError::new(
                InitialRejection::Missing,
                "session/load response has no _meta object",
            )
        })?;
    let history = meta
        .get(peri_acp_types::history_v2::HISTORY_META_KEY)
        .ok_or_else(|| {
            InitialAdmissionError::new(
                InitialRejection::Missing,
                "session/load response has no _meta[\"peri.history\"]",
            )
        })?;

    reject_mixed_fields(response, meta)?;

    let initial: HistoryInitialResult = serde_json::from_value(history.clone()).map_err(|_| {
        InitialAdmissionError::new(
            InitialRejection::Invalid,
            "history initial result has an invalid shape",
        )
    })?;
    initial.validate().map_err(|error| {
        InitialAdmissionError::new(InitialRejection::Invalid, error.to_string())
    })?;
    validate_page(&initial.page, expected_session, limits)?;

    let page = &initial.page;
    let segment = HistoryV2Segment::from_page(page, page.items.clone());
    let retained_items = segment.items.len();
    let retained_bytes = segment.bytes;
    let coverage = match &page.coverage {
        HistoryCoverage::EmptySnapshot => Vec::new(),
        HistoryCoverage::Interval(interval) => vec![interval.clone()],
    };
    let paging = HistoryV2Paging {
        view_epoch: 1,
        home: Some(HistoryV2Home {
            before_cursor: page.before_cursor.clone(),
            after_cursor: page.after_cursor.clone(),
            anchor: None,
        }),
        live: HistoryV2LiveState::from_boundary(&initial.live_boundary),
        last_request_key: Some(initial.request_key),
        retention: HistoryV2Retention::for_limits(limits),
        ..HistoryV2Paging::default()
    };

    Ok(HistoryV2State {
        status: HistoryV2Status::Ready,
        session_id: Some(expected_session.to_owned()),
        execution_ready: initial.execution_ready,
        runtime_generation: Some(initial.runtime_generation),
        delivery_id: Some(initial.delivery_id),
        live_boundary: Some(initial.live_boundary.clone()),
        window: Some(HistoryV2Window {
            session_id: expected_session.to_owned(),
            window_id: page.window_id.clone(),
            snapshot_token: page.snapshot_token.clone(),
            read_ticket: page.read_ticket.clone(),
            history_revision: page.history_revision,
            committed_tail: page.committed_tail,
            projection_version: page.projection_version,
            before_cursor: page.before_cursor.clone(),
            after_cursor: page.after_cursor.clone(),
            items: page.items.clone(),
            coverage,
            empty_snapshot: matches!(page.coverage, HistoryCoverage::EmptySnapshot),
            gaps: Vec::new(),
            segments: VecDeque::from([segment]),
            retained_items,
            retained_bytes,
            paging,
            progress: page.progress.clone(),
        }),
        failure: None,
    })
}

/// V2 响应中不允许出现的 V1 交付字段。
fn reject_mixed_fields(
    response: &Value,
    meta: &serde_json::Map<String, Value>,
) -> Result<(), InitialAdmissionError> {
    for legacy in ["snapshotToken", "nextCursor"] {
        if response.get(legacy).is_some() {
            return Err(InitialAdmissionError::new(
                InitialRejection::MixedFields,
                format!("V2 load response must not carry top-level {legacy}"),
            ));
        }
    }
    if meta.contains_key("periLoadGeneration") {
        return Err(InitialAdmissionError::new(
            InitialRejection::MixedFields,
            "V2 load response must not carry _meta.periLoadGeneration",
        ));
    }
    Ok(())
}

fn validate_page(
    page: &HistoryWindowPage,
    expected_session: &str,
    limits: &HistoryLimits,
) -> Result<(), InitialAdmissionError> {
    validate_window_page(page, expected_session, limits).map_err(|error| match error {
        PageValidationError::SessionMismatch => InitialAdmissionError::new(
            InitialRejection::SessionMismatch,
            "initial page belongs to another session",
        ),
        PageValidationError::Invalid(detail) => {
            InitialAdmissionError::new(InitialRejection::Invalid, detail)
        }
    })
}

/// H5c：V2 新会话的第一个窗口。
///
/// 与 [`admit_open_response`] 共用窗口构造，但允许从空状态建立：`session/new` 没有
/// `session/load` initial，open 响应本身就是首窗。不伪造 delivery/live 边界。
pub fn admit_new_session_open(
    session_id: &str,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
    request_key: peri_acp_types::history_v2::DecimalU64,
) -> Result<HistoryV2State, InitialAdmissionError> {
    let previous = HistoryV2State {
        session_id: Some(session_id.to_owned()),
        execution_ready: true,
        ..HistoryV2State::default()
    };
    admit_open_response(&previous, session_id, page, limits, request_key)
}

/// H5c：`window_expired` / `window_closed` / `snapshot_invalidated` 之后用
/// `_peri/history` 的 `open` 重建浏览窗口。
///
/// 新 open 只替换历史窗口；执行事实、delivery 与 live 边界沿用同会话的旧状态。
/// 失败时调用方保留旧窗口内容，只把页面状态标为可重试/需重开。
pub fn admit_open_response(
    previous: &HistoryV2State,
    session_id: &str,
    page: &HistoryWindowPage,
    limits: &HistoryLimits,
    request_key: peri_acp_types::history_v2::DecimalU64,
) -> Result<HistoryV2State, InitialAdmissionError> {
    if previous.session_id.as_deref() != Some(session_id) {
        return Err(InitialAdmissionError::new(
            InitialRejection::SessionMismatch,
            "history open belongs to another session",
        ));
    }
    validate_page(page, session_id, limits)?;

    let segment = HistoryV2Segment::from_page(page, page.items.clone());
    let retained_items = segment.items.len();
    let retained_bytes = segment.bytes;
    let coverage = match &page.coverage {
        HistoryCoverage::EmptySnapshot => Vec::new(),
        HistoryCoverage::Interval(interval) => vec![interval.clone()],
    };
    // 新窗口沿同一连接继续使用导航代次，旧 page/body 的迟到响应按 epoch 丢弃。
    let view_epoch = previous
        .window
        .as_ref()
        .map(|window| window.paging.view_epoch.saturating_add(1))
        .unwrap_or(1);
    let mut paging = previous
        .window
        .as_ref()
        .map(|window| window.paging.clone())
        .unwrap_or_default();
    paging.view_epoch = view_epoch;
    paging.in_flight = None;
    paging.reserved_bytes = 0;
    paging.home = Some(HistoryV2Home {
        before_cursor: page.before_cursor.clone(),
        after_cursor: page.after_cursor.clone(),
        anchor: None,
    });
    paging.anchor = None;
    paging.status = HistoryV2PagingStatus::Ready;
    paging.last_error = None;
    paging.last_request_key = Some(request_key);
    paging.bodies.clear();
    paging.retained_body_bytes = 0;
    paging.retention = HistoryV2Retention::for_limits(limits);

    Ok(HistoryV2State {
        status: HistoryV2Status::Ready,
        session_id: Some(session_id.to_owned()),
        execution_ready: previous.execution_ready,
        runtime_generation: previous.runtime_generation.clone(),
        delivery_id: previous.delivery_id.clone(),
        live_boundary: previous.live_boundary.clone(),
        window: Some(HistoryV2Window {
            session_id: session_id.to_owned(),
            window_id: page.window_id.clone(),
            snapshot_token: page.snapshot_token.clone(),
            read_ticket: page.read_ticket.clone(),
            history_revision: page.history_revision,
            committed_tail: page.committed_tail,
            projection_version: page.projection_version,
            before_cursor: page.before_cursor.clone(),
            after_cursor: page.after_cursor.clone(),
            items: page.items.clone(),
            coverage,
            empty_snapshot: matches!(page.coverage, HistoryCoverage::EmptySnapshot),
            gaps: Vec::new(),
            segments: VecDeque::from([segment]),
            retained_items,
            retained_bytes,
            paging,
            progress: page.progress.clone(),
        }),
        failure: None,
    })
}

/// H5b 的 reducer 入口：把已接纳的窗口身份交给状态模型，避免测试/调用方各自拼装。
pub fn initial_window(state: &HistoryV2State) -> Option<&HistoryV2Window> {
    state.window()
}
