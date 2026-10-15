use std::collections::HashMap;

use ratatui_kit::prelude::State;
use serde_json::json;

use crate::application::operations::{CompletionGate, OperationScope, OperationTicket};

pub(super) type ActionGates = HashMap<String, CompletionGate>;

pub(super) fn current_scope() -> OperationScope {
    OperationScope::new(
        ACTIVE_SESSION_ID.state().read().clone(),
        BRIDGE_RESET_COUNTER.get(),
    )
}

pub(super) fn reset_scope(gates: &mut ActionGates, scope: OperationScope) {
    gates.retain(|_, gate| !gate.reset_scope(scope.clone()));
}

use crate::{
    acp_client::AcpTuiClient,
    i18n,
    kit::atoms::{
        ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, CRON_ACTION_ERROR, CRON_JOBS,
    },
};

pub(super) fn dispatch(
    method: &'static str,
    id: &str,
    scope: OperationScope,
    gates: State<ActionGates>,
) {
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        fail(i18n::tr("panel-cron-no-client"));
        return;
    };
    dispatch_with_client(client, method, id, scope, gates);
}

fn dispatch_with_client(
    client: std::sync::Arc<AcpTuiClient>,
    method: &'static str,
    id: &str,
    scope: OperationScope,
    gates: State<ActionGates>,
) {
    let Some(identity) = client
        .stable_session_identity()
        .filter(|owner| scope.session_id() == owner.0 && scope == current_scope())
    else {
        fail(i18n::tr("panel-cron-no-session"));
        return;
    };
    let ticket = {
        let mut gates = gates.write();
        let gate = gates.entry(id.to_string()).or_default();
        if gate.is_pending() && gate.scope_matches(&scope) {
            tracing::debug!(id, method, "duplicate cron operation skipped");
            return;
        }
        gate.begin(scope)
    };
    CRON_ACTION_ERROR.set(None);
    // 单测接缝只接管发送；真实发送调度分支仍需人工/E2E 覆盖。
    #[cfg(test)]
    if tests::intercept_request(&ticket) {
        return;
    }
    let id = id.to_string();
    tokio::spawn(async move {
        // 发送前复检投影和目标；不持锁等待，不接管远端变更的生命周期。
        if ticket.is_cancelled() || ticket.scope() != &current_scope() {
            tracing::debug!(id, method, "stale cron operation skipped before send");
            return;
        }
        if !CRON_JOBS.state().read().iter().any(|job| job.id == id) {
            complete(
                gates,
                &ticket,
                &id,
                Err(peri_acp::transport::types::AcpError::new(
                    -32602,
                    "cron target no longer available",
                )),
            );
            return;
        }
        let result = request(&client, &identity, method, &id).await;
        if client.stable_session_identity().as_ref() != Some(&identity) {
            tracing::warn!(
                id,
                method,
                ?result,
                "cron result dropped: client identity changed"
            );
            return;
        }
        complete(gates, &ticket, &id, result);
    });
}

fn complete(
    gates: State<ActionGates>,
    ticket: &OperationTicket,
    id: &str,
    result: Result<(), peri_acp::transport::types::AcpError>,
) -> bool {
    // 即使新帧 effect 尚未执行，也不能让旧作用域结果发布到当前投影。
    if ticket.scope() != &current_scope() {
        tracing::warn!(id, ?result, "cron result dropped: scope changed");
        return false;
    }
    let Some(mut gates) = gates.try_write() else {
        tracing::warn!(
            id,
            ?result,
            "cron result dropped: observer gone, outcome unknown"
        );
        return false;
    };
    let Some(gate) = gates.get_mut(id) else {
        tracing::debug!(id, "cron result dropped: no pending target");
        return false;
    };
    if !gate.accept(ticket) {
        tracing::debug!(id, "cron result dropped: stale ticket");
        return false;
    }
    // 保留该目标的门及请求代次，避免同作用域再次发起时旧票与新票撞代。
    match result {
        Ok(()) => CRON_ACTION_ERROR.set(None),
        Err(error) => fail(i18n::tr_args(
            "panel-cron-action-failed",
            &[(
                "detail".into(),
                fluent_bundle::FluentValue::from(error.to_string()),
            )],
        )),
    }
    true
}

async fn request(
    client: &AcpTuiClient,
    identity: &(String, u64),
    method: &str,
    id: &str,
) -> Result<(), peri_acp::transport::types::AcpError> {
    let response = client
        .send_session_request(
            identity,
            method,
            json!({
                "sessionId": identity.0, "id": id,
            }),
        )
        .await?;
    if response.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || response.get("id").and_then(serde_json::Value::as_str) != Some(id)
    {
        return Err(peri_acp::transport::types::AcpError::new(
            -32603,
            "invalid cron operation response",
        ));
    }
    Ok(())
}

fn fail(message: String) {
    tracing::warn!(%message, "cron panel operation unavailable");
    CRON_ACTION_ERROR.set(Some(message));
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod tests;
