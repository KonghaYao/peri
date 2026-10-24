//! `initialize` 的 peri 能力收敛：客户端声明必须与后端实际支持一致。
//!
//! 只回显客户端声明会把「客户端想要」冒充成「端到端可用」。History V2 依赖 initial 与
//! `_peri/history` 的读取端口。未实现的后端（remote/emscripten）不得因声明而被协商成可用。

use peri_acp_types::history_v2::{
    ExecutionEffect, HistoryClientCapability, HistoryContractError, HistoryEffect,
    HistoryEffectState, HistoryErrorData, HistoryLimits, HistoryServerCapability,
};
use peri_acp_types::PeriCaps;
use serde_json::{Map, Value};

use crate::transport::types::AcpError;

use super::super::AcpServerConfig;
use super::session_lifecycle::{initialize_client_meta, initialize_peri_caps};

/// History V2 领域错误统一使用的 JSON-RPC server error code（设计 §12.1）。
pub(crate) const HISTORY_DOMAIN_ERROR_CODE: i64 = -32000;

/// 已删除的旧私有历史能力键：只作为**拒绝边界**存在。
const LEGACY_HISTORY_PAGINATION_KEY: &str = "peri.historyPagination";
const LEGACY_HISTORY_WINDOW_V1_KEY: &str = "peri.historyWindowV1";

/// 客户端声明收敛到当前后端的实际能力。
pub(crate) fn effective_peri_caps(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<PeriCaps, AcpError> {
    effective_caps_for_backend(
        initialize_peri_caps(params),
        &initialize_client_meta(params),
        cfg.session_resources.supports_history_v2(),
    )
}

/// 纯能力收敛规则：客户端声明不能把未实现的后端协商成可用。
///
/// History V2 的失败必须显式返回（`UnsupportedVersion`），不能静默清空
/// `caps.history_v2` 后把连接当作标准模式。
pub(crate) fn effective_caps_for_backend(
    mut caps: PeriCaps,
    meta: &Map<String, Value>,
    supports_history_v2: bool,
) -> Result<PeriCaps, AcpError> {
    caps.history_v2 = negotiate_history_v2(meta, supports_history_v2, HistoryLimits::default())
        .map_err(history_contract_error)?;
    Ok(caps)
}

/// History V2 纯协商规则（设计 §4 / §4.7 / §4.8）。
///
/// - `_meta` 无 `"peri.history"`：`Ok(None)`，旧路径不变；
/// - 形状错误：`InvalidRequest`；
/// - 同时声明旧私有能力：`UnsupportedVersion`（不得接受混合声明）；
/// - 版本/模式不受支持，或后端未兑现能力：`UnsupportedVersion`。
///
/// 只要客户端请求了 `peri.history`，本函数就不会返回「无声明」的 `None`：
/// 协商失败必须让 `initialize` 显式失败，不能转入标准模式。
pub(crate) fn negotiate_history_v2(
    meta: &Map<String, Value>,
    supports_history_v2: bool,
    limits: HistoryLimits,
) -> Result<Option<HistoryServerCapability>, HistoryContractError> {
    let Some(client) = HistoryClientCapability::parse_from_meta(meta)? else {
        return Ok(None);
    };
    if legacy_history_declared(meta) {
        return Err(HistoryContractError::UnsupportedVersion);
    }
    let server = client.select(&limits)?;
    if !supports_history_v2 {
        // 服务端不得回显自己无法兑现的能力，也不回退到标准模式。
        return Err(HistoryContractError::UnsupportedVersion);
    }
    Ok(Some(server))
}

/// 客户端是否同时声明了已知旧私有能力（显式 true 才算声明）。
fn legacy_history_declared(meta: &Map<String, Value>) -> bool {
    [LEGACY_HISTORY_PAGINATION_KEY, LEGACY_HISTORY_WINDOW_V1_KEY]
        .into_iter()
        .any(|key| meta.get(key).and_then(Value::as_bool) == Some(true))
}

/// `HistoryContractError` → `AcpError` 的唯一映射点。
///
/// 领域失败统一 `-32000`；`data` 是校验过的 [`HistoryErrorData`]。initialize
/// 阶段尚未发布任何历史，effect 因此固定为 `notReady` / `notPublished`。
fn history_contract_error(error: HistoryContractError) -> AcpError {
    let message = match &error {
        HistoryContractError::UnsupportedVersion => {
            "History V2 is not supported by this server".to_owned()
        }
        HistoryContractError::InvalidRequest { .. } => {
            "History V2 capability declaration is invalid".to_owned()
        }
        HistoryContractError::Limits { .. } => "History V2 limits are invalid".to_owned(),
        HistoryContractError::IdentityTooLong { .. } => {
            "History V2 identity exceeds its wire limit".to_owned()
        }
        HistoryContractError::Shape { .. } => "History V2 message shape is invalid".to_owned(),
    };
    let data = HistoryErrorData::new(
        error.reason(),
        error.recovery(),
        HistoryEffect {
            execution: ExecutionEffect::NotReady,
            history: HistoryEffectState::NotPublished,
            runtime_generation: None,
        },
    );
    if let Err(invalid) = data.validate() {
        // 契约自身不一致属于实现缺陷：仍返回领域错误码，但不发送未通过校验的 data。
        tracing::error!(error = %invalid, "history error data failed validation");
        return AcpError::new(HISTORY_DOMAIN_ERROR_CODE, message);
    }
    AcpError::new(HISTORY_DOMAIN_ERROR_CODE, message)
        .with_data(serde_json::to_value(&data).expect("HistoryErrorData 序列化不应失败"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::history_v2::{
        HistoryErrorReason, HistoryRecovery, HISTORY_CHANGED_NOTIFICATION, HISTORY_DOMAIN,
        HISTORY_META_KEY, HISTORY_METHOD, HISTORY_PROFILE_WINDOWED, HISTORY_V2_VERSION,
    };
    use serde_json::json;

    fn v2_request() -> Map<String, Value> {
        let mut request = Map::new();
        request.insert(
            HISTORY_META_KEY.to_owned(),
            json!({ "versions": [2], "requestedProfile": "windowed" }),
        );
        request
    }

    fn negotiate(
        meta: &Map<String, Value>,
        supports_history_v2: bool,
    ) -> Result<Option<HistoryServerCapability>, HistoryContractError> {
        negotiate_history_v2(meta, supports_history_v2, HistoryLimits::default())
    }

    fn error_reason(error: &AcpError) -> HistoryErrorReason {
        let data = error.data.as_ref().expect("领域错误必须携带 data");
        serde_json::from_value::<HistoryErrorData>(data.clone())
            .expect("data 必须是合法的 HistoryErrorData")
            .reason
    }

    #[test]
    fn no_history_declaration_keeps_caps_and_declares_no_v2() {
        let caps = PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        };
        let caps = effective_caps_for_backend(caps, &Map::new(), true).unwrap();
        assert!(caps.history_v2.is_none());
        assert!(caps.rewind, "无 V2 声明时其他能力不变");
    }

    #[test]
    fn windowed_v2_on_supported_backend_returns_server_capability() {
        let server = negotiate(&v2_request(), true).unwrap().unwrap();
        assert_eq!(server.version, HISTORY_V2_VERSION);
        assert_eq!(server.selected_profile, HISTORY_PROFILE_WINDOWED);
        assert_eq!(server.method, HISTORY_METHOD);
        assert_eq!(server.notification, HISTORY_CHANGED_NOTIFICATION);
        assert_eq!(server.limits, HistoryLimits::default());
    }

    #[test]
    fn unsupported_backend_rejects_v2_instead_of_echoing_it() {
        let error = negotiate(&v2_request(), false).unwrap_err();
        assert_eq!(error, HistoryContractError::UnsupportedVersion);

        let acp = history_contract_error(error);
        assert_eq!(acp.code, HISTORY_DOMAIN_ERROR_CODE);
        let data = serde_json::to_value(acp.data.expect("data")).unwrap();
        assert_eq!(data["domain"], HISTORY_DOMAIN);
        assert_eq!(data["version"], HISTORY_V2_VERSION);
        assert_eq!(data["reason"], "unsupported_version");
        assert_eq!(data["recovery"], "renegotiate");
        assert_eq!(data["effect"]["execution"], "notReady");
        assert_eq!(data["effect"]["history"], "notPublished");
        assert!(
            data["effect"].get("runtimeGeneration").is_some()
                && data["effect"]["runtimeGeneration"].is_null(),
            "非 ready 的 effect 必须显式输出 runtimeGeneration:null: {data}"
        );
        assert_eq!(
            serde_json::from_value::<HistoryErrorData>(data)
                .unwrap()
                .reason,
            HistoryErrorReason::UnsupportedVersion
        );
        assert_eq!(
            serde_json::from_value::<HistoryErrorData>(
                serde_json::to_value(HistoryContractError::UnsupportedVersion.to_error_data())
                    .unwrap()
            )
            .unwrap()
            .recovery,
            HistoryRecovery::Renegotiate
        );
    }

    #[test]
    fn unsupported_versions_or_profile_are_rejected() {
        let mut wrong_version = Map::new();
        wrong_version.insert(
            HISTORY_META_KEY.to_owned(),
            json!({ "versions": [1], "requestedProfile": "windowed" }),
        );
        assert_eq!(
            negotiate(&wrong_version, true).unwrap_err(),
            HistoryContractError::UnsupportedVersion
        );

        let mut wrong_profile = Map::new();
        wrong_profile.insert(
            HISTORY_META_KEY.to_owned(),
            json!({ "versions": [2], "requestedProfile": "full" }),
        );
        assert_eq!(
            negotiate(&wrong_profile, true).unwrap_err(),
            HistoryContractError::UnsupportedVersion
        );
    }

    #[test]
    fn mixed_legacy_and_v2_declarations_are_rejected() {
        for legacy in ["peri.historyPagination", "peri.historyWindowV1"] {
            let mut mixed = v2_request();
            mixed.insert(legacy.to_owned(), Value::Bool(true));
            let error = negotiate(&mixed, true).unwrap_err();
            assert_eq!(
                error,
                HistoryContractError::UnsupportedVersion,
                "混合声明必须拒绝: {legacy}"
            );
            assert_eq!(
                error_reason(&history_contract_error(error)),
                HistoryErrorReason::UnsupportedVersion
            );
            assert!(
                effective_caps_for_backend(PeriCaps::default(), &mixed, true).is_err(),
                "混合声明不得产生任何 server cap: {legacy}"
            );
        }
    }

    #[test]
    fn legacy_only_history_declarations_do_not_select_or_echo_v2() {
        for legacy in ["peri.historyPagination", "peri.historyWindowV1"] {
            let mut meta = Map::new();
            meta.insert(legacy.to_owned(), Value::Bool(true));
            assert!(
                negotiate_history_v2(&meta, true, HistoryLimits::default())
                    .unwrap()
                    .is_none(),
                "只声明已删除的旧能力不得被当成 V2: {legacy}"
            );
            let caps = PeriCaps::from_client_meta(&meta);
            assert!(caps.history_v2.is_none());
            assert!(
                caps.to_agent_meta().get(legacy).is_none(),
                "已删除的旧能力不得被回显: {legacy}"
            );
        }
    }

    #[test]
    fn malformed_or_empty_history_declarations_are_rejected() {
        for value in [
            json!(true),
            json!("windowed"),
            json!({ "versions": [2], "requestedProfile": "windowed", "unknown": 1 }),
            json!({ "versions": [], "requestedProfile": "windowed" }),
        ] {
            let mut request = Map::new();
            request.insert(HISTORY_META_KEY.to_owned(), value);
            assert!(
                negotiate(&request, true).is_err(),
                "请求了 peri.history 但声明不可用时必须显式失败"
            );
        }
    }

    #[test]
    fn to_agent_meta_includes_v2_only_when_negotiated() {
        let announced = PeriCaps {
            history_v2: Some(HistoryServerCapability {
                version: HISTORY_V2_VERSION,
                selected_profile: HISTORY_PROFILE_WINDOWED.to_owned(),
                method: HISTORY_METHOD.to_owned(),
                notification: HISTORY_CHANGED_NOTIFICATION.to_owned(),
                limits: HistoryLimits::default(),
            }),
            ..PeriCaps::default()
        };
        let meta = announced.to_agent_meta();
        let history = &meta[HISTORY_META_KEY];
        assert_eq!(history["version"], HISTORY_V2_VERSION);
        assert_eq!(history["selectedProfile"], HISTORY_PROFILE_WINDOWED);
        assert_eq!(history["method"], HISTORY_METHOD);
        assert_eq!(history["notification"], HISTORY_CHANGED_NOTIFICATION);
        assert_eq!(history["limits"], json!(HistoryLimits::default()));

        assert!(
            !PeriCaps::default()
                .to_agent_meta()
                .contains_key(HISTORY_META_KEY),
            "未协商时不得提前宣告 peri.history"
        );
    }
}
