//! 会话内按 server 身份协调 MCP 控制，完成后刷新宿主事实。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use peri_acp::transport::types::AcpError;
use ratatui_kit::prelude::State;
use serde_json::{Value, json};

use crate::acp_client::AcpTuiClient;
use crate::application::operations::{
    CompletionGate, OperationOutcome, OperationScope, OperationTicket, OutcomeUnknownReason,
};
use crate::i18n;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, MCP_SERVERS};

const MCP_CONTROL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ControlAction {
    Restart,
    Stop,
}

impl ControlAction {
    fn wire(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Stop => "stop",
        }
    }
}

#[derive(Default)]
struct ServerOperation {
    gate: CompletionGate,
    action: Option<ControlAction>,
    outcome: Option<OperationOutcome>,
    status: Option<String>,
}

#[derive(Default)]
pub(super) struct ControlState {
    servers: HashMap<String, ServerOperation>,
}

impl ControlState {
    pub(super) fn reset_scope(&mut self, scope: OperationScope) {
        self.servers
            .retain(|_, operation| !operation.gate.reset_scope(scope.clone()));
    }

    fn begin(
        &mut self,
        name: &str,
        action: ControlAction,
        scope: OperationScope,
    ) -> Option<OperationTicket> {
        let operation = self.servers.entry(name.into()).or_default();
        if operation.gate.is_pending() && operation.gate.scope_matches(&scope) {
            return None;
        }
        operation.action = Some(action);
        operation.outcome = None;
        operation.status = None;
        Some(operation.gate.begin(scope))
    }

    fn complete(
        &mut self,
        name: &str,
        ticket: &OperationTicket,
        outcome: OperationOutcome,
        status: Option<String>,
    ) -> bool {
        let Some(operation) = self.servers.get_mut(name) else {
            return false;
        };
        if !operation.gate.accept(ticket) {
            return false;
        }
        operation.action = None;
        operation.outcome = Some(outcome);
        operation.status = status;
        true
    }

    pub(super) fn cancel_wait(&mut self, name: &str) -> bool {
        let Some(operation) = self.servers.get_mut(name).filter(|op| op.gate.is_pending()) else {
            return false;
        };
        operation.gate.cancel();
        operation.action = None;
        operation.outcome = Some(OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::WaitCancelled,
        });
        true
    }

    pub(super) fn message(&self, name: &str) -> Option<String> {
        let operation = self.servers.get(name)?;
        if operation.gate.is_pending() {
            return Some(i18n::tr(match operation.action? {
                ControlAction::Restart => "panel-mcp-restarting",
                ControlAction::Stop => "panel-mcp-stopping",
            }));
        }
        operation
            .outcome
            .as_ref()
            .map(|outcome| outcome_message(outcome, operation.status.as_deref()))
    }
}

pub(super) fn current_scope() -> OperationScope {
    OperationScope::new(
        ACTIVE_SESSION_ID.state().read().clone(),
        BRIDGE_RESET_COUNTER.get(),
    )
}

pub(super) fn dispatch(
    name: &str,
    action: ControlAction,
    scope: OperationScope,
    state: State<ControlState>,
) {
    if scope != current_scope() {
        tracing::debug!(server = name, "stale MCP control event skipped");
        return;
    }
    let Some(client) = ACP_CLIENT_HANDLE
        .get()
        .cloned()
        .filter(|client| client.supports_mcp_control())
    else {
        super::notify(i18n::tr("panel-mcp-control-unavailable"));
        return;
    };
    let Some(identity) = client
        .stable_session_identity()
        .filter(|owner| owner.0 == scope.session_id())
    else {
        super::notify(i18n::tr("panel-mcp-oauth-start-no-session"));
        return;
    };
    if !target_allowed(name, action) {
        super::notify(i18n::tr("panel-mcp-control-target-unavailable"));
        return;
    }
    let Some(ticket) = state.write().begin(name, action, scope) else {
        return;
    };
    let reset = BRIDGE_RESET_COUNTER.get();
    tokio::spawn(run(
        client,
        identity,
        reset,
        name.to_string(),
        action,
        state,
        ticket,
    ));
}

fn target_allowed(name: &str, action: ControlAction) -> bool {
    MCP_SERVERS
        .state()
        .read()
        .iter()
        .find(|server| server.name == name)
        .is_some_and(|server| match action {
            ControlAction::Restart => server.control.restart,
            ControlAction::Stop => server.control.stop,
        })
}

async fn run(
    client: Arc<AcpTuiClient>,
    identity: (String, u64),
    reset: u64,
    name: String,
    action: ControlAction,
    state: State<ControlState>,
    ticket: OperationTicket,
) {
    // 事件帧与真正发送之间再次固定作用域与目标，禁止用旧下标/旧会话发请求。
    if ticket.is_cancelled() || ticket.scope() != &current_scope() {
        return;
    }
    let (mut outcome, status) = if !target_allowed(&name, action) {
        (
            OperationOutcome::Failed {
                reason: i18n::tr("panel-mcp-control-target-unavailable"),
            },
            None,
        )
    } else {
        tokio::select! {
            biased;
            _ = ticket.token().cancelled() => (OperationOutcome::OutcomeUnknown { reason: OutcomeUnknownReason::WaitCancelled }, None),
            result = request(&client, &identity, &name, action) => match result {
                Ok(status) => (OperationOutcome::Succeeded, Some(status)),
                Err(error) => {
                    tracing::warn!(session_id = %identity.0, server = %name, %error, "MCP control request failed");
                    (classify_error(error), None)
                }
            }
        }
    };
    let refresh =
        crate::kit::service_snapshot::refresh_mcp_projection(&client, &identity, reset).await;
    if let Err(error) = &refresh {
        tracing::warn!(session_id = %identity.0, server = %name, %error, "MCP control projection refresh failed");
        if outcome == OperationOutcome::Succeeded {
            outcome = OperationOutcome::OutcomeUnknown {
                reason: OutcomeUnknownReason::ObserverGone,
            };
        }
    }
    if matches!(refresh, Ok(false)) && outcome == OperationOutcome::Succeeded {
        outcome = OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::ObserverGone,
        };
    }
    if client.stable_session_identity().as_ref() != Some(&identity)
        || ticket.scope() != &current_scope()
    {
        tracing::warn!(session_id = %identity.0, server = %name, ?outcome, "MCP control observer changed; outcome unknown");
        return;
    }
    let Some(mut state) = state.try_write() else {
        tracing::warn!(server = %name, "MCP control observer gone; outcome unknown");
        return;
    };
    if state.complete(&name, &ticket, outcome.clone(), status.clone()) {
        drop(state);
        super::notify(outcome_message(&outcome, status.as_deref()));
    }
}

fn classify_error(error: AcpError) -> OperationOutcome {
    let kind = error.data.as_ref().and_then(|data| data.get("kind"));
    match (error.code, kind) {
        (-32603, Some(kind)) if kind == "request_timeout" => OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::TimedOut,
        },
        (-32603, None) => OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::ObserverGone,
        },
        _ => OperationOutcome::Failed {
            reason: error.message,
        },
    }
}

async fn request(
    client: &AcpTuiClient,
    identity: &(String, u64),
    name: &str,
    action: ControlAction,
) -> Result<String, AcpError> {
    let response = client
        .send_session_request_with_timeout(
            identity,
            "peri/mcp/control",
            json!({
                "sessionId": identity.0, "serverName": name, "action": action.wire(),
            }),
            MCP_CONTROL_TIMEOUT,
        )
        .await?;
    let status = response.get("status").and_then(Value::as_str);
    let valid = response.get("sessionId").and_then(Value::as_str) == Some(identity.0.as_str())
        && response.get("serverName").and_then(Value::as_str) == Some(name)
        && response.get("action").and_then(Value::as_str) == Some(action.wire())
        && match action {
            ControlAction::Restart => matches!(status, Some("connected" | "needsAuthorization")),
            ControlAction::Stop => status == Some("stopped"),
        };
    if !valid {
        return Err(AcpError::new(-32603, "invalid MCP control response")
            .with_data(json!({"kind":"invalid_response"})));
    }
    Ok(status.expect("validated MCP status").to_string())
}

fn outcome_message(outcome: &OperationOutcome, status: Option<&str>) -> String {
    match outcome {
        OperationOutcome::Succeeded => i18n::tr(match status {
            Some("stopped") => "panel-mcp-control-stopped",
            Some("needsAuthorization") => "panel-mcp-control-needs-auth",
            _ => "panel-mcp-control-connected",
        }),
        OperationOutcome::Failed { reason } => i18n::tr_args(
            "panel-mcp-control-failed",
            &[("detail".into(), reason.clone().into())],
        ),
        OperationOutcome::OutcomeUnknown { .. } => i18n::tr("panel-mcp-control-unknown"),
    }
}

pub(super) fn unavailable_message(reason: Option<&str>) -> String {
    i18n::tr(match reason {
        Some("builtin") => "panel-mcp-control-builtin",
        Some("workspace_remote") => "panel-mcp-control-workspace",
        Some("system_mcp") => "panel-mcp-control-system",
        Some("acp") => "panel-mcp-control-acp",
        Some("session_policy") => "panel-mcp-control-session-policy",
        _ => "panel-mcp-control-unavailable",
    })
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod tests;
