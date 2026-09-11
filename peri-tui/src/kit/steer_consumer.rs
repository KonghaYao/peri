use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use peri_acp::transport::types::AcpError;
use peri_acp_types::session::{
    DispatchUserInputsRequest, EnqueueUserInputRequest, TakeBackUserInputRequest,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use super::atoms;
use super::steer_state::{STEERS, SteerCommand, SteerCommandKind};
use crate::acp_client::AcpTuiClient;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(10);
const FAILURE_NOTICE_DURATION: Duration = Duration::from_secs(6);

pub(crate) fn spawn_steer_consumer(
    client: AcpTuiClient,
    mut receiver: UnboundedReceiver<SteerCommand>,
    cwd: String,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut retries: VecDeque<SteerCommand> = VecDeque::new();
        let mut warned = HashSet::new();
        let mut retry_tick = tokio::time::interval(RECONCILE_INTERVAL);
        retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let mut command = tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = retry_tick.tick() => {
                    let Some(command) = retries.pop_front().or_else(refresh_command) else {
                        continue;
                    };
                    command
                },
                command = receiver.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
            };
            let result = tokio::select! {
                _ = shutdown.cancelled() => break,
                result = tokio::time::timeout(RECEIPT_TIMEOUT, execute(&client, &mut command, &cwd)) => {
                    result.unwrap_or_else(|_| Err(AcpError::new(-32603, "user input receipt timed out")))
                },
            };
            if let Err(error) = result {
                let rejected = reject_command(&mut command, &error);
                if let Some(snapshot) = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("snapshot"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                {
                    STEERS
                        .state()
                        .write()
                        .accept_snapshot(snapshot, command.epoch, false);
                }
                tracing::warn!(code = error.code, command_id = %command.command_id, "user input command failed");
                if !matches!(command.kind, SteerCommandKind::Refresh)
                    && warned.insert(command.command_id.clone())
                {
                    atoms::NOTIFICATION.set(Some(atoms::Notification {
                        message: crate::i18n::tr(if rejected {
                            "steer-input-rejected"
                        } else {
                            "steer-input-uncertain"
                        }),
                        until: std::time::Instant::now() + FAILURE_NOTICE_DURATION,
                    }));
                }
                if !rejected && atoms::BRIDGE_RESET_COUNTER.get() == command.epoch {
                    retries.push_back(command);
                }
            }
        }
    })
}

fn refresh_command() -> Option<SteerCommand> {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    if session_id.is_empty() || !super::steer_state::is_enabled() {
        return None;
    }
    Some(SteerCommand {
        session_id,
        epoch: atoms::BRIDGE_RESET_COUNTER.get(),
        command_id: uuid::Uuid::now_v7().to_string(),
        generation: None,
        kind: SteerCommandKind::Refresh,
    })
}

fn reject_command(command: &mut SteerCommand, error: &AcpError) -> bool {
    let not_admitted =
        matches!(command.kind, SteerCommandKind::Enqueue(_)) && command.generation.is_none();
    if not_admitted && command.session_id.is_empty() {
        let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
        let epoch = atoms::BRIDGE_RESET_COUNTER.get();
        STEERS
            .state()
            .write()
            .rebind_initial(command, &session_id, epoch);
        command.session_id = session_id;
        command.epoch = epoch;
    }
    // 入队请求尚未发送时，会话准备失败是确定未受理；不能把原稿卡在未知回执中。
    let rejected = not_admitted || matches!(error.code, -32602..=-32600);
    STEERS.state().write().reject(command, rejected);
    rejected
}

async fn execute(
    client: &AcpTuiClient,
    command: &mut SteerCommand,
    cwd: &str,
) -> Result<(), AcpError> {
    if command.session_id.is_empty() && matches!(command.kind, SteerCommandKind::Enqueue(_)) {
        let session_id = client.ensure_session(cwd, None).await?;
        let epoch = atoms::BRIDGE_RESET_COUNTER.get();
        STEERS
            .state()
            .write()
            .rebind_initial(command, &session_id, epoch);
        command.session_id = session_id;
        command.epoch = epoch;
    }
    if !matches!(command.kind, SteerCommandKind::Refresh) {
        let pending_epoch = STEERS
            .state()
            .read()
            .pending_command(&command.session_id, &command.command_id)
            .map(|pending| pending.epoch);
        let Some(epoch) = pending_epoch else {
            return Ok(());
        };
        command.epoch = epoch;
    }
    if atoms::ACTIVE_SESSION_ID.state().read().as_str() != command.session_id
        || atoms::BRIDGE_RESET_COUNTER.get() != command.epoch
    {
        return Err(AcpError::new(
            if command.generation.is_some() {
                -32603
            } else {
                -32602
            },
            "user input session changed before admission",
        ));
    }
    let cached_generation = STEERS
        .state()
        .read()
        .snapshot(&command.session_id, command.epoch)
        .map(|snapshot| snapshot.generation.clone());
    let current_generation = match cached_generation {
        Some(generation) if !matches!(command.kind, SteerCommandKind::Refresh) => generation,
        _ => {
            let snapshot = client.user_input_snapshot(&command.session_id).await?;
            let generation = snapshot.generation.clone();
            super::steer_state::establish_session_snapshot(snapshot);
            generation
        }
    };
    if command
        .generation
        .as_ref()
        .is_some_and(|generation| generation != &current_generation)
    {
        // 旧重试可能仍在 consumer 内；实例改变后保留未知结果，不重投或恢复重复稿。
        return Ok(());
    }
    let generation = command.generation.get_or_insert(current_generation).clone();
    STEERS.state().write().bind_command_generation(command);
    let receipt = match &command.kind {
        SteerCommandKind::Refresh => return Ok(()),
        SteerCommandKind::Enqueue(input) => {
            client
                .enqueue_user_input(&EnqueueUserInputRequest {
                    session_id: command.session_id.clone(),
                    generation,
                    command_id: command.command_id.clone(),
                    input_id: input.input_id.clone(),
                    content: input.content.clone(),
                    original_draft: input.original_draft.clone(),
                })
                .await?
        }
        SteerCommandKind::Dispatch(ids) => {
            client
                .dispatch_user_inputs(&DispatchUserInputsRequest {
                    session_id: command.session_id.clone(),
                    generation,
                    command_id: command.command_id.clone(),
                    input_ids: ids.clone(),
                })
                .await?
        }
        SteerCommandKind::TakeBack { id, .. } => {
            client
                .take_back_user_input(&TakeBackUserInputRequest {
                    session_id: command.session_id.clone(),
                    generation,
                    command_id: command.command_id.clone(),
                    input_id: id.clone(),
                })
                .await?
        }
    };
    STEERS.state().write().settle(command, receipt);
    Ok(())
}

#[cfg(test)]
#[path = "steer_consumer_test.rs"]
mod tests;
