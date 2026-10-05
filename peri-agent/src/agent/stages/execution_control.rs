use peri_acp_types::session_resources::{
    ControlAction, ControlAttempt, ControlCommand, ControlDecision, ControlStatus,
};

use super::{LoopResult, StageContext};

#[cfg(test)]
#[path = "execution_control_test.rs"]
mod tests;

pub async fn run_react_loop(context: StageContext, max_iterations: usize) -> LoopResult {
    let port = context.session.transcript.read().idempotent_reminder_port();
    let binding = context.session.turn.execution_binding();
    let attempt = ControlAttempt {
        turn_id: binding.turn_id,
        attempt_id: binding.attempt_id,
    };
    if let Some((resources, session_id, _)) = &port {
        let admitted = async {
            let current = resources.load_session_control(session_id).await?;
            if current.status != ControlStatus::Active {
                return Err(
                    peri_acp_types::session_resources::SessionResourceError::conflict(
                        "session activation is paused or closed",
                    ),
                );
            }
            let receipt = resources
                .apply_session_control(&ControlCommand {
                    session_id: session_id.clone(),
                    command_id: format!("execution-enter:{}", attempt.attempt_id.as_str()),
                    expected_lifecycle: current.lifecycle,
                    expected_revision: current.revision,
                    expected_control_generation: current.control_generation,
                    action: ControlAction::ObserveAttempt {
                        target: Some(attempt.clone()),
                    },
                })
                .await?;
            if receipt.decision != ControlDecision::Accepted {
                return Err(
                    peri_acp_types::session_resources::SessionResourceError::conflict(
                        "execution entry control changed",
                    ),
                );
            }
            if !context
                .session
                .turn
                .bind_control_generation(receipt.state.control_generation)
            {
                return Err(
                    peri_acp_types::session_resources::SessionResourceError::conflict(
                        "execution control generation is immutable",
                    ),
                );
            }
            Ok(())
        }
        .await;
        if let Err(error) = admitted {
            return LoopResult::Error(anyhow::anyhow!("execution control blocked: {error}").into());
        }
    }
    let result = super::run_react_loop_inner(context, max_iterations).await;
    if let Some((resources, session_id, _)) = port {
        let released = async {
            let current = resources.load_session_control(&session_id).await?;
            if current.attempt.as_ref() != Some(&attempt) {
                return Ok(());
            }
            let receipt = resources
                .apply_session_control(&ControlCommand {
                    session_id,
                    command_id: format!("execution-exit:{}", attempt.attempt_id.as_str()),
                    expected_lifecycle: current.lifecycle,
                    expected_revision: current.revision,
                    expected_control_generation: current.control_generation,
                    action: ControlAction::ObserveAttempt { target: None },
                })
                .await?;
            if receipt.decision != ControlDecision::Accepted {
                return Err(
                    peri_acp_types::session_resources::SessionResourceError::conflict(
                        "execution exit control changed",
                    ),
                );
            }
            Ok(())
        }
        .await;
        if let Err(error) = released {
            return LoopResult::Error(
                anyhow::anyhow!("execution quiescence unconfirmed: {error}").into(),
            );
        }
    }
    result
}

pub(super) async fn validate(context: &StageContext) -> crate::error::AgentResult<()> {
    let Some(generation) = context.session.turn.control_generation() else {
        return Ok(());
    };
    let port = context.session.transcript.read().idempotent_reminder_port();
    let (resources, session_id, _) =
        port.ok_or_else(|| anyhow::anyhow!("bound execution control resources unavailable"))?;
    let state = resources
        .load_session_control(&session_id)
        .await
        .map_err(|error| anyhow::anyhow!("execution control is unconfirmed: {error}"))?;
    let binding = context.session.turn.execution_binding();
    if context.session.turn.is_cancelled()
        || state.status != ControlStatus::Active
        || state.control_generation != generation
        || !state.attempt.is_some_and(|target| {
            target.turn_id == binding.turn_id && target.attempt_id == binding.attempt_id
        })
    {
        return Err(crate::error::AgentError::Interrupted);
    }
    Ok(())
}
