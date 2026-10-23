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
/// - 携带旧顶层私有字段：无论是否协商 V2 都在装配前 `-32602` 拒绝；
/// - 没有 meta：V2 连接显式拒绝（initial 必填）；标准连接返回 `None`，走标准完整回放；
/// - 有 meta：未协商 V2 → `protocol_not_negotiated`；`action` 不为 `initial`、未知字段、
///   版本或预算不合法 → `-32602`。
pub(crate) fn parse(
    params: &Value,
    facts: &ConnectionFacts,
) -> Result<Option<HistoryInitialRequest>, AcpError> {
    let raw = params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get(HISTORY_META_KEY));
    // 旧私有参数一律不再解释：即使标准连接也不能靠顶层 cursor/limit 触发旧分页。
    if let Some(field) = LEGACY_PRIVATE_FIELDS
        .iter()
        .find(|field| params.get(**field).is_some())
    {
        let reason = if facts.history_v2 && raw.is_some() {
            "history_initial_mixed_fields"
        } else {
            "history_legacy_fields_removed"
        };
        return Err(invalid_request(
            reason,
            format!("{field} is no longer supported on session/load"),
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::history_v2::{
        HistoryLimits, HistoryServerCapability, HISTORY_CHANGED_NOTIFICATION, HISTORY_METHOD,
    };
    use peri_acp_types::PeriCaps;

    fn v2_facts() -> ConnectionFacts {
        ConnectionFacts {
            history_v2: true,
            mcp_control_v1: false,
            caps: PeriCaps {
                history_v2: Some(HistoryServerCapability {
                    version: 2,
                    selected_profile: "windowed".into(),
                    method: HISTORY_METHOD.into(),
                    notification: HISTORY_CHANGED_NOTIFICATION.into(),
                    limits: HistoryLimits::default(),
                }),
                ..PeriCaps::default()
            },
            admission_caps: PeriCaps::default(),
        }
    }

    fn initial_params() -> Value {
        json!({
            "sessionId": "s1",
            "_meta": {
                "peri.history": {
                    "version": 2,
                    "action": "initial",
                    "requestKey": "1",
                    "limit": 50,
                    "targetBytes": 524_288,
                }
            }
        })
    }

    #[test]
    fn legacy_top_level_fields_are_rejected_for_both_connection_kinds() {
        for field in LEGACY_PRIVATE_FIELDS {
            let mut params = initial_params();
            params[field] = json!("legacy");
            let error = parse(&params, &v2_facts()).unwrap_err();
            assert_eq!(error.code, -32602, "{field}");
            assert_eq!(
                error.data.as_ref().and_then(|data| data["reason"].as_str()),
                Some("history_initial_mixed_fields"),
                "V2 连接混入旧字段: {field}"
            );

            let mut standard = json!({ "sessionId": "s1" });
            standard[field] = json!("legacy");
            let error = parse(&standard, &ConnectionFacts::unnegotiated()).unwrap_err();
            assert_eq!(error.code, -32602, "{field}");
            assert_eq!(
                error.data.as_ref().and_then(|data| data["reason"].as_str()),
                Some("history_legacy_fields_removed"),
                "标准连接不得再解释旧字段: {field}"
            );
        }

        let params = json!({ "sessionId": "s1" });
        assert!(
            parse(&params, &ConnectionFacts::unnegotiated())
                .unwrap()
                .is_none(),
            "标准连接不带旧字段时仍走标准 load"
        );
    }

    #[test]
    fn unnegotiated_initial_is_rejected_before_assembly() {
        let error = parse(&initial_params(), &ConnectionFacts::unnegotiated()).unwrap_err();
        assert_eq!(error.code, -32000);
    }
}
