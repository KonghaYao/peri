//! ACP Notification dispatch — handles incoming notifications and pushes
//! session update notifications. Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::sync::Arc;

use crate::dispatch::commands::{build_available_commands_update, register_ui_entries};
use crate::dispatch::config_update;
use crate::session::executor::ContinuationRequest;
use agent_client_protocol::schema::v1::SessionUpdate;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::session::MessageSource;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{debug, info};

use super::{
    continuation::cancel_arms_continuation, continuation::cancel_should_schedule_continuation,
    AcpServerConfig, SessionState,
};
use crate::provider::LlmProvider;

// ── Notification dispatch ────────────────────────────────────────────────────

/// 返回 `Some(ContinuationRequest)` 表示调用方需在 **sessions 锁外** 补发一次
/// continuation 通知（`session/cancel` race 兜底），其余通知返回 `None`。
pub(crate) fn handle_notification(
    method: &str,
    params: &Value,
    sessions: &mut HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
) -> Option<ContinuationRequest> {
    match method {
        "session/cancel" => {
            let session_id = extract_session_id(params, "");
            if let Some(state) = sessions.get_mut(session_id) {
                let token = state.cancel_token.as_ref()?;
                // 多读者 + 单 writer lease：cancel 是写入操作，仅 writer 可发起。
                // 协议无客户端身份字段，writer 恒为 session 创建方（"default"）——
                // 观察者（非 writer）的 cancel 请求被忽略（只读）。
                if !state.lease.is_writer("default") {
                    debug!(session_id = %session_id, "Cancel ignored: read-only observer");
                    return None;
                }
                token.cancel();
                // 置位内部续跑标记（只影响当前被取消的 prompt）：被取消 prompt
                // 的独立 bg agent 结果完成时，continuation scheduler 原子 take
                // 后运行一次 AsyncContinuation，使父 agent 消费 deferred callback。
                // 仅在确有 prompt 在跑时置位（cancel_token 存在），避免无关的
                // bg 完成误触发；用户显式新 prompt 会清除未运行的标记。
                //
                // 取消**正在执行的 continuation** 时不置位（in_flight）：否则
                // 会形成"取消续跑 → 再次续跑"的自动链式续跑。被取消续跑遗留的
                // Defer 保留在队列，由后续用户 prompt 消费。
                let cancel_arms = cancel_arms_continuation(state);
                if cancel_arms {
                    state.continuation_armed = true;
                }
                info!(session_id = %session_id, "Cancel requested");

                // Race 兜底：bg callback 可能已经 route 为 Defer/SubAgentComplete，
                // 且其 continuation 通知恰在 cancel 置位前被 scheduler 跳过
                // （armed=false）。此时若不补发，Defer 已入队却永远不会被消费。
                // 仅在队列确有 pending SubAgentComplete Defer 时补发，且 kind
                // 恒为 Agent——Shell/Workflow 虽可经 route_bg_result 入队，但使用
                // 各自独立的 MessageSource，不会产生 SubAgentComplete Defer，天然
                // 不会误触发。
                let has_pending = cfg
                    .session_manager
                    .get_session(session_id)
                    .map(|s| {
                        s.v2_message_queue
                            .has_pending_defer(&MessageSource::SubAgentComplete)
                    })
                    .unwrap_or(false);
                if cancel_should_schedule_continuation(state, has_pending) {
                    return Some(ContinuationRequest {
                        session_id: session_id.to_string(),
                        kind: BgTaskKind::Agent,
                    });
                }
            }
            None
        }
        "session/config_update" => {
            // Two formats:
            // 1. {"config": PeriConfig} — full config replace (from update_config)
            // 2. {"configId": "model"/"provider", "value": "..."} — partial (from set_config_option)
            if let Some(config_val) = params.get("config") {
                let new_cfg: crate::provider::PeriConfig = match serde_json::from_value(
                    config_val.clone(),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "config_update notification: invalid config");
                        return None;
                    }
                };
                let active_profile_provider = new_cfg
                    .config
                    .profiles
                    .get(&new_cfg.config.active_alias)
                    .map(|p| p.provider.as_str())
                    .unwrap_or("");
                tracing::info!(
                    active_provider = %active_profile_provider,
                    provider_count = new_cfg.config.providers.len(),
                    "config_update notification: full config replace"
                );
                *cfg.peri_config.write() = new_cfg.clone();
                if let Some(p) = LlmProvider::from_config(&new_cfg) {
                    *cfg.provider.write() = p;
                }
            } else if let (Some(config_id), Some(value)) = (
                params.get("configId").and_then(|v| v.as_str()),
                params.get("value").and_then(|v| v.as_str()),
            ) {
                match config_id {
                    "model" => {
                        let mut c = cfg.peri_config.write();
                        c.config.active_alias = value.to_string();
                        drop(c);
                        let new_provider = {
                            let c = cfg.peri_config.read();
                            LlmProvider::from_config_for_alias(&c, value)
                        };
                        if let Some(p) = new_provider {
                            tracing::info!(alias = %value, "config_update notification: model changed");
                            *cfg.provider.write() = p;
                        }
                    }
                    other => {
                        tracing::debug!(config_id = %other, "config_update notification: unhandled configId");
                    }
                }
            } else {
                tracing::debug!("config_update notification: missing config/configId");
            }
            // No sessions to invalidate — pool will be built fresh on next session/new
            None
        }
        _ => {
            debug!(method = %method, "Unhandled notification");
            None
        }
    }
}

// ── Notification helpers ───────────────────────────────────────────────────────

/// Extract `sessionId` from JSON-RPC params, returning `default_value` if absent.
pub(crate) fn extract_session_id<'a>(params: &'a Value, default_value: &'a str) -> &'a str {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or(default_value)
}

/// Build the current set of config options and push a `ConfigOptionUpdate` notification.
pub(crate) async fn send_config_option_update(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    cfg: &AcpServerConfig,
) {
    if session_id.is_empty() {
        return;
    }
    let update = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        SessionUpdate::ConfigOptionUpdate(config_update::make_config_option_update(
            &c,
            &p,
            cfg.permission_mode.load(),
        ))
    };
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize ConfigOptionUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// Push an `AvailableCommandsUpdate` notification for the given session.
///
/// Phase 6 A4 起投影数据源 = 注册表 `snapshot()`（本地 skills（C1）/ ui
/// 明细 / 插件静态条目（B2）已由各自注册路径写入；MCP 条目由发现管线
/// （A3）异步注入）；**注册表 on_change 为投影重建重发的唯一触发源**。
/// 发送失败仅 error 日志，现有语义。
pub(crate) async fn send_available_commands_update(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    caps: &PeriCaps,
    command_registry: Option<Arc<CommandRegistry>>,
    stdio_command_filter: bool,
) {
    if session_id.is_empty() {
        return;
    }
    let Some(command_registry) = command_registry else {
        tracing::warn!(
            session_id,
            "send_available_commands_update: 无 session 级命令注册表，跳过广播"
        );
        return;
    };
    // stdio 部署过滤 rewind/clear（IDE 客户端自管理）：命令列表/补全不显示。
    // 仅 stdio_command_filter 为 true 时生效，其余命令与 TUI 完全一致。
    let filtered_snapshot = |reg: &CommandRegistry| {
        let entries: Vec<Arc<RouteEntry>> = reg
            .snapshot()
            .into_iter()
            .filter(|e| {
                !(stdio_command_filter
                    && matches!(e.fullname.as_str(), "core:rewind" | "core:clear"))
            })
            .collect();
        entries
    };
    // 时序契约（P2-6）：广播入口先摘除旧 on_change，再执行 ui 注册（一次性、
    // 幂等），最后挂新回调。首次广播时旧回调不存在（防双发约束不变）；
    // session/load 对同一 session 重广播时，ui 注册动作不再触发旧回调
    // （发往旧连接捕获的 transport/cx 快照）。
    command_registry.set_on_change(None);
    // 时序（防双发）：ui 注册（一次性、幂等）必须在 set_on_change 挂载之前
    // 完成；on_change 回调内直接重建 snapshot 投影，不重放 ui 注册（P1-1）。
    register_ui_entries(caps, &command_registry);
    let entries = filtered_snapshot(&command_registry);
    let update = build_available_commands_update(&entries, caps);

    // 注册表 on_change → 投影重建重发（唯一触发源）。防引用环：回调只捕获
    // Weak(command_registry) + 不可变快照数据；session 销毁（注册表无强引用）
    // 后 upgrade 失败即静默返回。
    let weak = Arc::downgrade(&command_registry);
    let tx = Arc::clone(transport);
    let caps_owned = caps.clone();
    let sid = session_id.to_string();
    let stdio_filter = stdio_command_filter;
    command_registry.set_on_change(Some(Arc::new(move || {
        let Some(reg) = weak.upgrade() else {
            return;
        };
        let tx = Arc::clone(&tx);
        let caps = caps_owned.clone();
        let sid = sid.clone();
        tokio::spawn(async move {
            let entries: Vec<Arc<RouteEntry>> = reg
                .snapshot()
                .into_iter()
                .filter(|e| {
                    !(stdio_filter && matches!(e.fullname.as_str(), "core:rewind" | "core:clear"))
                })
                .collect();
            let update = build_available_commands_update(&entries, &caps);
            let update_value =
                match serde_json::to_value(SessionUpdate::AvailableCommandsUpdate(update)) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "Failed to serialize AvailableCommandsUpdate"
                        );
                        return;
                    }
                };
            // Use {"update": ..., "sessionId": ...} format — same as TransportEventSink —
            // so that handle_session_update_peri on the TUI side can parse via params.get("update").
            let payload = serde_json::json!({
                "sessionId": sid,
                "update": update_value,
            });
            let _ = tx.send_notification("session/update", payload).await;
        });
    })));

    let update_value = match serde_json::to_value(SessionUpdate::AvailableCommandsUpdate(update)) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize AvailableCommandsUpdate");
            return;
        }
    };
    // Use {"update": ..., "sessionId": ...} format — same as TransportEventSink —
    // so that handle_session_update_peri on the TUI side can parse via params.get("update").
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}

/// Push a `SessionInfoUpdate` notification after prompt/compact completes,
/// or after a session rename.
pub(crate) async fn send_session_info_update(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
) {
    send_session_info_update_with_title(transport, session_id, None).await;
}

/// Push a `SessionInfoUpdate` notification with an optional title override.
/// Called from the `session/rename` handler and the prediction SetTitle flow.
pub(crate) async fn send_session_info_update_with_title(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    title: Option<&str>,
) {
    use agent_client_protocol::schema::v1::SessionInfoUpdate;
    let mut info = SessionInfoUpdate::new().updated_at(chrono::Utc::now().to_rfc3339());
    if let Some(t) = title {
        info = info.title(t.to_string());
    }
    let update = SessionUpdate::SessionInfoUpdate(info);
    let update_value = match serde_json::to_value(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize SessionInfoUpdate");
            return;
        }
    };
    let payload = serde_json::json!({
        "sessionId": session_id,
        "update": update_value,
    });
    let _ = transport.send_notification("session/update", payload).await;
}
// test
