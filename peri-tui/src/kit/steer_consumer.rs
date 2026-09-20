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
/// 宿主对「本节点没有该会话的执行所有权」的拒绝码（`WorkspaceError` → ACP `-32010`）。
const EXECUTION_OWNERSHIP_REQUIRED: i64 = -32010;
/// 受理回执期限：只覆盖已发出请求的等待。
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(10);
/// 准备阶段期限：会话创建包含工作区发现与服务端准入，比回执预算宽松。
///
/// 两个阶段不共用期限：准备慢于回执预算时输入尚未发出，
/// 按回执结论收尾会把「还没来得及发送」说成输入未被接收。
const PREPARE_TIMEOUT: Duration = Duration::from_secs(60);
const FAILURE_NOTICE_DURATION: Duration = Duration::from_secs(6);

/// 失败发生的阶段：会话没准备好与输入未被受理，对用户是不同的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SteerStage {
    Prepare,
    Admit,
}

/// 用户输入链路的失败，附带失败阶段。
#[derive(Debug)]
struct SteerFailure {
    error: AcpError,
    stage: SteerStage,
}

impl From<AcpError> for SteerFailure {
    fn from(error: AcpError) -> Self {
        Self {
            error,
            stage: SteerStage::Admit,
        }
    }
}

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
                result = execute(&client, &mut command, &cwd) => result,
            };
            if let Err(failure) = result {
                let SteerFailure { error, stage } = failure;
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
                tracing::warn!(code = error.code, stage = ?stage, command_id = %command.command_id, "user input command failed");
                if !matches!(command.kind, SteerCommandKind::Refresh)
                    && warned.insert(command.command_id.clone())
                {
                    let message = match (stage, ownership_denied_for_read_only_session(&error)) {
                        // 只读会话不是「回执不明」：说清原因，原稿按确定拒绝还回 composer。
                        (SteerStage::Admit, true) => crate::i18n::tr("steer-session-read-only"),
                        _ => failure_notice(stage, rejected, &error),
                    };
                    atoms::NOTIFICATION.set(Some(atoms::Notification {
                        message,
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

/// 失败提示文案。
///
/// 会话未能建立的失败发生在输入受理之前，服务端已给出原因：提示必须复述该原因，
/// 不能沿用「输入未被接收」这一结论——用户据此无法判断是输入被拒还是环境不可用。
/// 入队被拒或回执不明的结论保持原样。
fn failure_notice(stage: SteerStage, rejected: bool, error: &AcpError) -> String {
    match (stage, rejected) {
        (SteerStage::Prepare, true) => crate::i18n::tr_args(
            "steer-session-unavailable",
            &[("error".into(), error.message.clone().into())],
        ),
        (_, true) => crate::i18n::tr("steer-input-rejected"),
        (_, false) => crate::i18n::tr("steer-input-uncertain"),
    }
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
    let rejected = not_admitted
        || matches!(error.code, -32602..=-32600)
        || ownership_denied_for_read_only_session(error);
    STEERS.state().write().reject(command, rejected);
    rejected
}

/// 只读准入的会话上，宿主的 `-32010` 是确定结论（本会话没有执行所有权），不是「回执不明」。
///
/// 判据只是客户端已经持有的准入事实，不改分工：请求照发、结论由宿主的 `require_owner`
/// 给出，这里只决定向上呈现的口径——不把只读会话的提交挂在「等待回执」上无限重投，
/// 也不让原稿卡在待定态。
fn ownership_denied_for_read_only_session(error: &AcpError) -> bool {
    error.code == EXECUTION_OWNERSHIP_REQUIRED && atoms::SESSION_READ_ONLY.state().read().is_some()
}

/// 一次输入投递：先准备会话，再发送请求并等待受理回执。
///
/// 两个阶段各有自己的期限——准备阶段不被回执预算提前放弃，
/// 已发出的请求也不因准备耗时被误判成未受理。
async fn execute(
    client: &AcpTuiClient,
    command: &mut SteerCommand,
    cwd: &str,
) -> Result<(), SteerFailure> {
    prepare(client, command, cwd).await?;
    tokio::time::timeout(RECEIPT_TIMEOUT, admit(client, command))
        .await
        .unwrap_or_else(|_| Err(AcpError::new(-32603, "user input receipt timed out")))?;
    Ok(())
}

/// 准备首会话。失败发生在输入受理之前，因此有独立的阶段标记与期限。
///
/// 准备期间的可见状态由 `AcpTuiClient` 在会话建立时给出（`SESSION_PREPARING`），
/// 这里不重复投影：等待中的输入既可能自己发起建立，也可能只是在等应用启动的那次。
async fn prepare(
    client: &AcpTuiClient,
    command: &mut SteerCommand,
    cwd: &str,
) -> Result<(), SteerFailure> {
    if !command.session_id.is_empty() || !matches!(command.kind, SteerCommandKind::Enqueue(_)) {
        return Ok(());
    }
    let session_id =
        match tokio::time::timeout(PREPARE_TIMEOUT, client.ensure_session(cwd, None)).await {
            Ok(Ok(session_id)) => session_id,
            Ok(Err(error)) => {
                return Err(SteerFailure {
                    error,
                    stage: SteerStage::Prepare,
                });
            }
            Err(_) => {
                return Err(SteerFailure {
                    error: AcpError::new(-32603, "session preparation timed out"),
                    stage: SteerStage::Prepare,
                });
            }
        };
    let epoch = atoms::BRIDGE_RESET_COUNTER.get();
    STEERS
        .state()
        .write()
        .rebind_initial(command, &session_id, epoch);
    command.session_id = session_id;
    command.epoch = epoch;
    Ok(())
}

/// 核对实例身份后发送请求并落定回执。
async fn admit(client: &AcpTuiClient, command: &mut SteerCommand) -> Result<(), AcpError> {
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
