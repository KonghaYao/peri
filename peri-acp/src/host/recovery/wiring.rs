//! server loop 的恢复接线：进入、wire 视图、执行阻断与冷 child 运行期绑定。
//!
//! 协调器只负责装配与提醒接纳；把当前连接登记为 child endpoint、补 TaskManager
//! 绑定是传输层调用方的责任，因此集中在这里，避免 coordinator 反向依赖连接。

use std::sync::Arc;

use peri_acp_types::recovery::{RecoveryError, RecoveryReport, RecoveryView};
use peri_acp_types::thread::ThreadId;
use serde_json::Value;

use super::super::connection::ConnectionContext;
use super::super::session_endpoint::register_session_endpoint;
use super::super::{AcpServerConfig, SharedSessions};
use super::{EnteredRecovery, HostSessionRecovery};
use crate::transport::types::AcpError;

/// 进入会话时触发恢复的 setup 方法。`session/fork` 派生新会话，不恢复旧工作。
///
/// `session/new` 不在此列：新 thread 的工作意图恒为 `idle`，进入在响应之后完成
/// （见 `ServerLoop::dispatch_request`），避免恢复准备推迟会话响应。
pub(crate) fn starts_session_recovery(method: &str) -> bool {
    matches!(method, "session/load" | "session/resume")
}

/// 会话 setup 方法涉及的会话 id：`session/new` | `session/fork` 取响应里的新 id，
/// `session/load` | `session/resume` 取请求里的 `sessionId`；非 setup 方法与失败的
/// 调用返回 `None`（失败响应不做后置初始化）。
pub(crate) fn session_setup(
    method: &str,
    params: &Value,
    response: Option<&Value>,
) -> Option<String> {
    let response = response?;
    match method {
        "session/new" | "session/fork" => response
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "session/load" | "session/resume" => {
            let session_id = super::super::extract_session_id(params, "");
            (!session_id.is_empty()).then(|| session_id.to_owned())
        }
        _ => None,
    }
}

/// 进入已装配会话的恢复入口：thread 身份取自已发布的 `SessionState`。
///
/// 没有发布 thread 身份时不进入恢复（只记日志）：恢复必须绑定到确切的 thread，
/// 不能拿 session id 猜。
pub(crate) async fn enter_recovery(
    recovery: &HostSessionRecovery,
    sessions: &SharedSessions,
    session_id: &str,
) -> Result<Option<EnteredRecovery>, RecoveryError> {
    let thread_id = {
        let states = sessions.lock().await;
        states.get(session_id).map(|state| state.thread_id.clone())
    };
    let Some(thread_id) = thread_id else {
        tracing::warn!(
            session_id = %session_id,
            "session recovery skipped: no thread identity was published"
        );
        return Ok(None);
    };
    recovery
        .enter_prepared(&ThreadId::from(thread_id))
        .await
        .map(Some)
}

/// 恢复失败的 wire 视图：entered thread 取失败分类携带的 thread，否则回落会话 id。
pub(crate) fn error_view(session_id: &str, error: &RecoveryError) -> RecoveryView {
    let entered = error
        .thread_id()
        .cloned()
        .unwrap_or_else(|| ThreadId::from(session_id.to_owned()));
    RecoveryView::from_error(&entered, error)
}

/// 仅在实际恢复发生或失败时附加 `_meta["peri.recovery"]`；普通 idle load 没有噪声字段。
pub(crate) fn attach_recovery_meta(result: &mut Result<Value, AcpError>, view: &RecoveryView) {
    if !view.should_attach() {
        return;
    }
    let Ok(response) = result else {
        return;
    };
    let Some(object) = response.as_object_mut() else {
        return;
    };
    let Some(meta) = object
        .entry("_meta")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
    else {
        return;
    };
    if let Ok(value) = serde_json::to_value(view) {
        meta.insert("peri.recovery".to_owned(), value);
    }
}

/// 每次新的 load/resume 以本次结果重设执行阻断：成功或可重试失败清除旧阻断，
/// 只有不可执行分类才设置。阻断仅在内存，随会话 runtime 释放。
pub(crate) fn apply_recovery_execution_block(
    cfg: &AcpServerConfig,
    session_id: &str,
    view: &RecoveryView,
) {
    let Some(runtime) = cfg.session_manager.get_session(session_id) else {
        tracing::warn!(
            session_id,
            "recovery execution block was not applied: session runtime is missing"
        );
        return;
    };
    match view.error.as_ref() {
        Some(failure) if view.execution_blocked => {
            runtime.set_recovery_execution_block(failure.clone())
        }
        _ => runtime.clear_recovery_execution_block(),
    }
}

/// 恢复成功后补齐每个已装配 thread 的 TaskManager 绑定与在线 endpoint。
///
/// 放在响应发出之前：响应之后的 launch 启动 child 时，MCP 工具与审批必须已经可达。
/// `assemble_for_recovery` 只装配，不经 server loop 的恢复入口需要自行调用本 helper。
pub(crate) async fn bind_recovered_sessions(
    cfg: &Arc<AcpServerConfig>,
    sessions: &SharedSessions,
    connection: &Arc<tokio::sync::Mutex<ConnectionContext>>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    report: &RecoveryReport,
) {
    let threads = report
        .recovered
        .iter()
        .chain(&report.reused)
        .chain(report.skipped.iter().map(|skip| &skip.thread_id));
    for thread_id in threads {
        let Some(session_id) = cfg.session_manager.session_id_for_thread(thread_id) else {
            continue;
        };
        let environment = {
            let states = sessions.lock().await;
            states
                .get(&session_id)
                .and_then(|state| state.environment.clone())
        };
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        crate::host::requests::bind_session_tasks(&session_id, local, transport);
        register_session_endpoint(sessions, &session_id, connection, transport).await;
    }
}
