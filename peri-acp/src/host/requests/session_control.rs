use std::collections::HashMap;

use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, ControlReceipt, ControlResolution,
    ControlState, ControlStatus,
};
use peri_acp_types::thread::ThreadId;
use serde_json::{json, Value};

use super::super::{workspace::resource_error, AcpServerConfig, SessionState};
use crate::transport::types::AcpError;

pub(super) async fn handle(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    if method == "session/control/state" {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
        let state = cfg
            .session_resources
            .load_session_control(&ThreadId::from(id))
            .await
            .map_err(resource_error)?;
        let settlement = match state.status {
            ControlStatus::Closed => json!({"status":"settled"}),
            ControlStatus::Closing => json!({"status":"pending"}),
            _ => json!({"status":"pending"}),
        };
        return Ok(json!({"state":state,"settlement":settlement}));
    }
    let command: ControlCommand = serde_json::from_value(params.clone())
        .map_err(|error| AcpError::new(-32602, format!("invalid control command: {error}")))?;
    if matches!(
        command.action,
        ControlAction::FinishClose | ControlAction::ObserveAttempt { .. }
    ) {
        return Err(AcpError::new(
            -32602,
            "internal control action is not callable",
        ));
    }
    command.digest().map_err(resource_error)?;
    if method == "session/control/resolve" {
        let resolution = cfg
            .session_resources
            .resolve_session_control(&command)
            .await
            .map_err(resource_error)?;
        if let ControlResolution::Applied { receipt } = &resolution {
            reconcile_effects(cfg, sessions, &command, receipt).await?;
        }
        return serde_json::to_value(resolution)
            .map_err(|error| AcpError::new(-32603, error.to_string()));
    }
    let receipt = cfg
        .session_resources
        .apply_session_control(&command)
        .await
        .map_err(resource_error)?;
    reconcile_effects(cfg, sessions, &command, &receipt).await?;
    serde_json::to_value(receipt).map_err(|error| AcpError::new(-32603, error.to_string()))
}

async fn reconcile_effects(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    command: &ControlCommand,
    receipt: &ControlReceipt,
) -> Result<(), AcpError> {
    if receipt.decision == ControlDecision::Accepted {
        let current = cfg
            .session_resources
            .load_session_control(&command.session_id)
            .await
            .map_err(resource_error)?;
        if current == receipt.state
            || (command.action == ControlAction::Close
                && current.status == ControlStatus::Closing
                && current.lifecycle == receipt.state.lifecycle
                && current.control_generation == receipt.state.control_generation)
        {
            apply_effects(cfg, sessions, command, &current).await?;
        }
    }
    Ok(())
}

async fn apply_effects(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    command: &ControlCommand,
    current: &ControlState,
) -> Result<(), AcpError> {
    let id = command.session_id.as_str();
    match &command.action {
        ControlAction::Pause | ControlAction::Stop { .. } => {
            if let Some(state) = sessions.get_mut(id) {
                state.continuation_mq_steering_pending = false;
                state.continuation_epoch = state.continuation_epoch.saturating_add(1);
                if let Some(token) = &state.cancel_token {
                    token.cancel();
                }
            }
            if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(id) {
                mailbox.stop();
            }
            cfg.session_manager.cancel_session(id);
        }
        ControlAction::Close => {
            if let Err(error) =
                super::session_lifecycle::close_session(cfg, sessions, id, false).await
            {
                tracing::warn!(session_id = id, %error, "domain close remains incomplete");
                return Ok(());
            }
            let settled = cfg
                .session_resources
                .load_session_control(&command.session_id)
                .await
                .map_err(resource_error)?;
            if settled.lifecycle != current.lifecycle
                || settled.control_generation != current.control_generation
            {
                return Err(AcpError::new(
                    -32010,
                    "Close generation changed before settlement",
                ));
            }
            cfg.session_resources
                .apply_session_control(&ControlCommand {
                    session_id: command.session_id.clone(),
                    command_id: format!(
                        "close-settled:{}",
                        command.digest().map_err(resource_error)?
                    ),
                    expected_lifecycle: settled.lifecycle,
                    expected_revision: settled.revision,
                    expected_control_generation: settled.control_generation,
                    action: ControlAction::FinishClose,
                })
                .await
                .map_err(resource_error)?;
        }
        ControlAction::Resume => {}
        ControlAction::Reopen => {
            if let Some(state) = sessions.get_mut(id) {
                state.closing = false;
            }
        }
        ControlAction::FinishClose | ControlAction::ObserveAttempt { .. } => unreachable!(),
    }
    Ok(())
}
