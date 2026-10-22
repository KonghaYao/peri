//! `session/load` 的 History V2 initial 准入（设计 §4.4、§6.1、§12.1）。
//!
//! 解析与校验发生在会话生命周期装配**之前**：V2 连接必须显式请求 `initial`；请求形状、
//! 版本与预算错误按 `-32602` 拒绝；initial 与旧顶层私有历史字段互斥。未协商 V2 的连接
//! 携带 initial 时使用领域错误 `protocol_not_negotiated`，不能静默忽略或降级回 V1。
//!
//! 本模块只做 wire 准入与计划装配，不读取存储、不创建窗口；真正的 initial 由
//! [`super::history_service::HistoryV2Service::initial`] 执行。

use std::sync::Arc;

use peri_acp_types::history_v2::{HistoryInitialRequest, HistoryLimits, HISTORY_META_KEY};
use serde_json::{json, Value};

use super::connection::ConnectionFacts;
use super::history_service::{protocol_not_negotiated_error, HistoryV2Service};
use crate::transport::types::AcpError;

/// 与 initial 互斥的旧顶层私有字段（设计 §4.4）。
const LEGACY_PRIVATE_FIELDS: [&str; 4] = ["cursor", "limit", "loadGeneration", "history"];

/// 已通过准入、等待生命周期装配后执行的 initial。
pub(crate) struct LoadInitial {
    pub(crate) request: HistoryInitialRequest,
    pub(crate) service: Arc<HistoryV2Service>,
    pub(crate) limits: HistoryLimits,
    /// 本连接的实时投递注册表：initial 成功装配时绑定到会话，供执行轮 sink 使用。
    pub(crate) live: Arc<super::live_delivery::LiveDelivery>,
}

/// 解析 `session/load` 的 `_meta["peri.history"]`。
///
/// - 没有 meta：V2 连接按设计显式拒绝；旧连接返回 `None`，保持 V1 行为。
/// - 有 meta：未协商 V2 → `protocol_not_negotiated`；混入旧顶层私有字段 → `-32602`；
///   `action` 不为 `initial`、未知字段、版本或预算不合法 → `-32602`。
pub(crate) fn parse(
    params: &Value,
    facts: &ConnectionFacts,
) -> Result<Option<HistoryInitialRequest>, AcpError> {
    let raw = params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(HISTORY_META_KEY));
    let Some(raw) = raw else {
        if facts.history_v2 {
            return Err(invalid_request(
                "history_initial_required",
                "session/load on a History V2 connection requires _meta[\"peri.history\"]",
            ));
        }
        return Ok(None);
    };
    if !facts.history_v2 {
        // 未协商的连接不能靠一次 load 隐式切换到 V2；也不得把 initial 当作未知元数据忽略。
        return Err(protocol_not_negotiated_error());
    }
    for field in LEGACY_PRIVATE_FIELDS {
        if params.get(field).is_some() {
            return Err(invalid_request(
                "history_initial_mixed_fields",
                format!("{field} is mutually exclusive with _meta[\"peri.history\"]"),
            ));
        }
    }
    let limits = limits(facts);
    let request = HistoryInitialRequest::decode(raw).map_err(invalid_params)?;
    request.validate(&limits).map_err(invalid_params)?;
    Ok(Some(request))
}

/// 本连接协商出的 History V2 预算。
pub(crate) fn limits(facts: &ConnectionFacts) -> HistoryLimits {
    facts
        .caps
        .history_v2
        .as_ref()
        .map(|capability| capability.limits.clone())
        .unwrap_or_default()
}

fn invalid_params(error: peri_acp_types::history_v2::HistoryContractError) -> AcpError {
    // 契约错误文本只含字段名/规则，不回显任何历史正文。
    AcpError::new(-32602, error.to_string())
}

fn invalid_request(reason: &'static str, message: impl Into<String>) -> AcpError {
    AcpError::new(-32602, message).with_data(json!({ "reason": reason }))
}
