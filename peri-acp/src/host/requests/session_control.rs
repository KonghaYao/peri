use std::collections::HashMap;

use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, ControlReceipt, ControlResolution,
    ControlState, ControlStatus,
};
use peri_acp_types::thread::ThreadId;
use serde_json::{json, Value};

use super::super::{workspace::resource_error, AcpServerConfig, SessionState};
use crate::transport::types::AcpError;

#[path = "session_control/stop_work.rs"]
mod stop_work;

#[path = "session_control/reopen.rs"]
mod reopen;

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
            || (matches!(
                command.action,
                ControlAction::Pause | ControlAction::Stop { .. }
            ) && current.status == ControlStatus::Paused
                && current.lifecycle == receipt.state.lifecycle
                && current.control_generation == receipt.state.control_generation)
        {
            apply_effects(cfg, sessions, command, &current).await?;
            if matches!(command.action, ControlAction::Stop { .. }) {
                stop_work::abandon_owned_work(cfg.session_resources.as_ref(), command, receipt)
                    .await?;
            }
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
                if let Some(token) = &state.cancel_token {
                    token.cancel();
                }
            }
            if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(id) {
                if matches!(command.action, ControlAction::Stop { .. }) {
                    mailbox
                        .reclaim_unclaimed_durable(&command.command_id, current.control_generation)
                        .await
                        .map_err(|error| {
                            AcpError::new(
                                -32010,
                                format!("Stop input settlement remains unconfirmed: {error}"),
                            )
                        })?;
                }
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
            let completion = ControlCommand {
                session_id: command.session_id.clone(),
                command_id: format!(
                    "close-settled:{}",
                    command.digest().map_err(resource_error)?
                ),
                expected_lifecycle: current.lifecycle,
                expected_revision: current.revision,
                expected_control_generation: current.control_generation,
                action: ControlAction::FinishClose,
            };
            let expected =
                peri_acp_types::session_resources::control::decide_control(&completion, current);
            if expected.decision != ControlDecision::Accepted || settled != expected.state {
                return Err(AcpError::new(
                    -32010,
                    "Close generation changed before settlement",
                ));
            }
        }
        ControlAction::Resume => {}
        ControlAction::Reopen => {
            reopen::apply(cfg, sessions, command, current).await?;
        }
        ControlAction::FinishClose | ControlAction::ObserveAttempt { .. } => unreachable!(),
    }
    Ok(())
}
