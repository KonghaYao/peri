//! 提交消费者——SUBMIT_TX channel → acp_client.prompt()。
//!
//! 这是 S4 的核心：InputArea 写入 channel 的用户文本在此被取出，转成
//! `MessageContent::text(...)` 并调用 `AcpTuiClient::prompt()`。同时承担
//! **首次会话懒初始化**：用户第一次提交时如果还没有 session，先创建。
//!
//! 设计：
//! - 单读取者，从 `mpsc::UnboundedReceiver<SubmitRequest>` 取请求；用户消息按
//!   会话车道串行（同会话 FIFO，跨会话并行），控制/视图请求即时执行
//! - 与 notifier / bridge 并行运行，三者通过独立 channel 解耦
//! - shutdown 信号触发时干净退出，不发残留请求

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use fluent_bundle::FluentValue;
use peri_acp_types::messages::MessageContent;
use ratatui::text::Line;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::{AcpTuiClient, AdmissionGuard};
use crate::i18n;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{
    ACP_STATE, EXIT_REQUESTED, LOADING_EPOCH, LOCAL_EVENT_TX, NOTIFICATION, RENDER_HEARTBEAT,
    REWIND_ACTION_TX,
};
use crate::kit::submit_request::{
    ExportMode, SessionControlRequest, SubmitRequest, ViewActionRequest,
};

use super::submit_admission::{
    CohortMode, CohortState, CohortTicket, Cohorts, LaneKey, SessionIdentity, SubmitAdmission,
    register_admission,
};

/// 取消通知的等待上限；超时不把未知结果当作已停止。
const CANCEL_RPC_TIMEOUT_SECS: u64 = 2;

/// 单会话提交车道：同会话按入队顺序串行，跨会话互不等待。
///
/// 旧服务端不支持 user-input queue：运行期输入缓存于 INPUT_BUFFER，TurnDone
/// 一次排空。通道只保证入队顺序；`tokio::spawn` 后每条提交由 runtime 并行
/// 调度，相邻 prompt 可能反向到达服务端——本地气泡按原顺序显示，模型却按
/// 另一顺序处理（#210）。车道以提交时的会话为键，让后一条等前一条结束；
/// 新式 steer 走独立 consumer，不受这里的串行约束。
pub(super) struct SubmitLanes<K> {
    tails: HashMap<K, JoinHandle<()>>,
}

impl<K: Eq + Hash> SubmitLanes<K> {
    pub(super) fn new() -> Self {
        Self {
            tails: HashMap::new(),
        }
    }

    /// 把 `work` 追加到 `key` 车道末尾。
    ///
    /// 前一条失败、panic 或被取消都只结束它自己，不阻断后续消息。
    pub(super) fn push(&mut self, key: K, work: impl Future<Output = ()> + Send + 'static) {
        // 已完成车道的句柄没有等待价值，顺手收敛 map（会话切换会换 key）。
        self.tails.retain(|_, tail| !tail.is_finished());
        let previous = self.tails.remove(&key);
        let handle = tokio::spawn(async move {
            if let Some(previous) = previous
                && let Err(error) = previous.await
            {
                warn!(error = %error, "kit submit_consumer: previous submit task aborted");
            }
            work.await;
        });
        self.tails.insert(key, handle);
    }

    /// `key` 车道是否还有未完成的提交。
    pub(super) fn is_active(&self, key: &K) -> bool {
        self.tails.get(key).is_some_and(|tail| !tail.is_finished())
    }
}

/// 启动提交消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient（Clone + Arc 内部）
/// - `rx`：SUBMIT_TX 的接收端（由 entry::run_kit_fullscreen 创建 channel 时拿到）
/// - `cwd`：用于首次 new_session 时传给 ACP server
/// - `shutdown`：与 notifier / bridge 共享的同一 CancellationToken
pub fn spawn_submit_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<SubmitRequest>,
    cwd: String,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit submit_consumer: started");
        let mut lanes: SubmitLanes<LaneKey> = SubmitLanes::new();
        let mut cohorts: Cohorts = Cohorts::new();
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit submit_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit submit_consumer: SUBMIT_TX dropped, exiting");
                            break;
                        }
                        Some(request) => {
                            let clear_session = matches!(
                                &request,
                                SubmitRequest::SessionControl(SessionControlRequest::Clear)
                            );
                            // 用户消息按会话串行：旧队列排空后每条都要等前一条
                            // 的 prompt 完成，否则模型侧顺序可能与气泡相反
                            // （#210）。控制/视图请求保持即时执行——它们不是
                            // 消息，不能被长 turn 拖住。
                            //
                            // 车道身份在入队时固定：切换/clear 后迟到的提交仍
                            // 留在原车道，执行前由 prompt 准入校验拒绝，不会改投
                            // 新会话（P1）。身份尚未落定的提交按 cohort 分道，
                            // 切换期间的新输入不会复用旧会话的采纳槽（P2）。
                            //
                            // 车道只服务旧服务端的直接 prompt 路径；新式服务端
                            // 由 session inbox 自管顺序与挂起注入，占用车道会让
                            // 一个 suspended prompt 挡住后续 KeepGoing/remote
                            // command，使其到不了注入路径。
                            let (lane, admission) = match &request {
                                SubmitRequest::AgentText { .. } | SubmitRequest::KeepGoing => {
                                    if acp_client.supports_user_input_queue() {
                                        // 新式服务端不占车道，但远程命令仍可能
                                        // 在没有会话时提交：无稳定身份就记下决策
                                        // 序号，稍后在守卫下建立/等待会话。
                                        let binding = acp_client.stable_session_identity();
                                        let unbound_switch_seq =
                                            binding.is_none().then(|| acp_client.session_switch_seq());
                                        (
                                            None,
                                            Some(SubmitAdmission {
                                                binding,
                                                cohort: None,
                                                unbound_switch_seq,
                                            }),
                                        )
                                    } else {
                                        cohorts.retain(|key, _| {
                                            lanes.is_active(&LaneKey::Cohort(*key))
                                        });
                                        let (lane, admission) =
                                            register_admission(&acp_client, &mut cohorts, &lanes);
                                        (Some(lane), Some(admission))
                                    }
                                }
                                _ => (None, None),
                            };
                            // 测试需要确认某条提交已被 consumer 接收并完成分道，
                            // 生产构建不编译。带上文本标签，避免并行用例互相干扰。
                            #[cfg(test)]
                            {
                                let label = match &request {
                                    SubmitRequest::AgentText { text, .. } => Some(text.clone()),
                                    SubmitRequest::KeepGoing => Some("<keepgoing>".to_string()),
                                    _ => None,
                                };
                                if let (Some(lane), Some(label)) = (lane.as_ref(), label) {
                                    admission_probe::note_registration(lane, label);
                                }
                            }
                            let client = acp_client.clone();
                            let cwd = cwd.clone();
                            let work = async move {
                                if let Err(e) =
                                    handle_submit(&client, &cwd, request, admission.as_ref()).await
                                {
                                    error!(error = %e, "kit submit_consumer: prompt failed");
                                    if clear_session {
                                        clear_loading_state();
                                    }
                                }
                            };
                            match lane {
                                Some(key) => lanes.push(key, work),
                                None => {
                                    tokio::spawn(work);
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 处理单条提交：确保 session 存在 → 发送 prompt。
/// `/clear`（及别名 `/cls` `/reset`）不经过 agent，直接创建新会话。
async fn handle_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
    request: SubmitRequest,
    admission: Option<&SubmitAdmission>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match request {
        SubmitRequest::AgentText { text, attachments } => {
            handle_agent_text_submit(acp_client, cwd, text, attachments, admission).await
        }
        SubmitRequest::KeepGoing => handle_keepgoing_submit(acp_client, cwd, admission).await,
        SubmitRequest::SessionControl(SessionControlRequest::Clear) => {
            handle_clear_submit(acp_client, cwd).await
        }
        SubmitRequest::SessionControl(SessionControlRequest::Rewind(args)) => {
            info!("kit submit_consumer: /rewind intercepted, forwarding to rewind_consumer");
            if let Some(tx) = REWIND_ACTION_TX.get() {
                let _ = tx.send(crate::kit::rewind_action::RewindAction::Preview {
                    target_message_id: args.target_message_id,
                    target_text: String::new(),
                });
            }
            Ok(())
        }
        SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup) => {
            warn!("kit submit_consumer: unexpected ToggleSetup request in consumer");
            Ok(())
        }
        SubmitRequest::ViewAction(action) => {
            info!(action = ?action, "kit submit_consumer: view-layer command intercepted");
            let execution_cwd = acp_client
                .current_execution_cwd()
                .unwrap_or_else(|| cwd.to_string());
            execute_view_action(action, acp_client, &execution_cwd);
            Ok(())
        }
        SubmitRequest::OpenPanel(kind) => {
            warn!(
                ?kind,
                "kit submit_consumer: unexpected OpenPanel request in consumer"
            );
            Ok(())
        }
    }
}

async fn handle_clear_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("kit submit_consumer: /clear intercepted, creating new session");

    // `/clear` intentionally remains unconditional. The client-owned operation
    // gate serializes it with startup/load/prompt lifecycle decisions.
    acp_client.new_session(cwd, None).await?;
    Ok(())
}

async fn handle_agent_text_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
    text: String,
    attachments: Vec<crate::kit::atoms::PendingAttachment>,
    admission: Option<&SubmitAdmission>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 入队时绑定的会话已切换，或所属 cohort 已作废：显式取消，不得改投新会话（P1）。
    if admission.is_some_and(|admission| admission_invalidated(acp_client, admission)) {
        notify_cancelled_submit();
        return Ok(());
    }

    if acp_client.supports_user_input_queue() && !crate::kit::input_area::is_remote_command(&text) {
        if let Err(error) = crate::kit::steer_state::enqueue(text, attachments) {
            warn!(error = %error, "user input enqueue failed; draft retained for recovery");
        }
        return Ok(());
    }
    let trimmed = text.trim();
    if trimmed.is_empty() && attachments.is_empty() {
        return Ok(());
    }

    // 身份在本 cohort 内落定：ensure/切换与采纳共用 operation gate，落定后
    // 不会再读到另一个 generation（P1）。
    let Some(identity) = resolve_admission_identity(acp_client, cwd, admission).await? else {
        notify_cancelled_submit();
        return Ok(());
    };
    #[cfg(test)]
    admission_probe::pause(&identity).await;
    if acp_client.stable_session_identity().as_ref() != Some(&identity) {
        // 身份在落定后被切走：本 cohort 剩余消息都必须取消，不能重新初始化。
        if let Some(cohort) = admission.and_then(|admission| admission.cohort.as_ref()) {
            cohort.cancel();
        }
        notify_cancelled_submit();
        return Ok(());
    }

    // Issue 2026-08-05 返工：在发送 PromptSubmitted 之前生成本轮 prompt 的
    // request_id（uuid v7）——PromptSubmitted 事件与 prompt RPC 必须携带同一
    // id，bridge 才能把它记录为"当前 turn 的 id"，供 stale TurnInterrupted 配对。
    let request_id = uuid::Uuid::now_v7().to_string();

    // 通过 LOCAL_EVENT_TX 发送 PromptSubmitted 事件到 acp_bridge，
    // 由 bridge 统一管理 phase/variant/is_loading 状态。
    {
        if let Some(tx) = LOCAL_EVENT_TX.get() {
            let _ = tx.send(AcpEventWithEpoch {
                child_origin: None,
                view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
                event: AcpEventData::PromptSubmitted {
                    request_id: Some(request_id.clone()),
                },
                active_session_id: identity.0.clone(),
            });
        } else {
            warn!("LOCAL_EVENT_TX not initialized, PromptSubmitted event dropped");
        }
    }
    // 递增 loading epoch：message_area 据此检测新一轮 loading 会话，
    // 避免 rapid toggle（如 TurnDone → drain_input_buffer → 新 prompt）
    // 在同一渲染周期内完成时组件错过 is_loading 的 false 中间态。
    *LOADING_EPOCH.state().write() += 1;
    tracing::info!(
        target: "msg_scroll_diag",
        "submit_consumer: emitted PromptSubmitted event, LOADING_EPOCH incremented, about to call prompt()",
    );

    // 上传式附件（图片 base64）与文本同一 content 上行；无附件时逐字等价于
    // 原有的 MessageContent::text（不改变无附件提交的在线形态）。
    let content = crate::kit::steer_state::content_with_attachments(trimmed, &attachments);
    let result = acp_client
        .prompt_in_session(&content, Some(request_id.clone()), &identity)
        .await;
    if let Err(e) = result {
        warn!(session_id = %identity.0, request_id = %request_id, error = ?e, "kit submit_consumer: prompt RPC failed");
        reset_failed_prompt(&identity.0, &request_id);
        if e.code == crate::acp_client::SESSION_BINDING_CHANGED_CODE {
            // 准入时身份已变化：显式取消并反馈，绝不改投。
            if let Some(cohort) = admission.and_then(|admission| admission.cohort.as_ref()) {
                cohort.cancel();
            }
            notify_cancelled_submit();
            return Ok(());
        }
        if e.code == crate::kit::RECOVERY_EXECUTION_BLOCKED_CODE {
            // 非 steer 的直接提交只有日志；恢复阻断必须让用户看到原因。
            // 该路径没有把文本放回输入框，文案不能声称「原稿已保留」。
            NOTIFICATION.set(Some(crate::kit::atoms::Notification {
                message: i18n::tr_args(
                    "prompt-recovery-blocked",
                    &[("error".into(), e.message.clone().into())],
                ),
                until: peri_time::monotonic_now() + Duration::from_secs(15),
            }));
        }
        return Err(Box::new(e));
    }
    Ok(())
}

/// 提交的准入是否已经失效：所属 cohort 已作废，或绑定身份已不是当前稳定会话。
fn admission_invalidated(acp_client: &AcpTuiClient, admission: &SubmitAdmission) -> bool {
    if admission
        .cohort
        .as_ref()
        .is_some_and(CohortTicket::is_cancelled)
    {
        return true;
    }
    admission
        .binding
        .as_ref()
        .is_some_and(|binding| acp_client.stable_session_identity().as_ref() != Some(binding))
}

/// 解析本次提交实际要写入的会话身份。
///
/// 已绑定提交直接沿用入队身份；尚未落定的提交由所属 cohort 的首条成员在
/// 生命周期同步内确定完整身份并写回 cohort，后续成员复用它。
/// `None` 表示身份已失效（切换取代/落定失败），调用方必须显式取消。
async fn resolve_admission_identity(
    acp_client: &AcpTuiClient,
    cwd: &str,
    admission: Option<&SubmitAdmission>,
) -> Result<Option<SessionIdentity>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(admission) = admission else {
        // 无准入信息（直接调用方）：按首次提交处理，身份不再跨临界区重读。
        return Ok(Some(acp_client.ensure_session_identity(cwd, None).await?));
    };
    if let Some(binding) = &admission.binding {
        return Ok(Some(binding.clone()));
    }
    let Some(cohort) = &admission.cohort else {
        // 新式服务端不占车道：远程命令在入队时的决策守卫下建立/等待会话，
        // 期间发生切换则取消，不改投（P1）。
        return match admission.unbound_switch_seq {
            Some(switch_seq) => Ok(acp_client
                .resolve_unbound_admission(cwd, switch_seq)
                .await?),
            None => Ok(None),
        };
    };
    match &*cohort.state.lock().unwrap() {
        CohortState::Adopted(identity) => return Ok(Some(identity.clone())),
        CohortState::Cancelled => return Ok(None),
        CohortState::Pending => {}
    }

    info!(cwd = %cwd, "kit submit_consumer: resolving session admission");
    let allow_create = matches!(cohort.mode, CohortMode::Establish);
    let guard = AdmissionGuard::new(cohort.switch_seq, allow_create);
    let mut resolved = None;
    acp_client
        .resolve_session_admission(cwd, guard, |identity| {
            let mut state = cohort.state.lock().unwrap();
            match identity {
                Some(identity) => {
                    resolved = Some(identity.clone());
                    *state = CohortState::Adopted(identity.clone());
                }
                None => *state = CohortState::Cancelled,
            }
        })
        .await?;
    Ok(resolved)
}

/// 会话切换导致排队提交被取消时的用户可见反馈。
fn notify_cancelled_submit() {
    *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message: i18n::tr("submit-cancelled-session-changed"),
        until: peri_time::monotonic_now() + Duration::from_secs(10),
    });
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

/// keepgoing 提交：向 ACP 发送空白 user prompt。
///
/// 与 `handle_agent_text_submit` 的差异：
/// - 不 trim/丢弃空文本——空白 prompt 是 keepgoing 的协议语义（服务端不插入
///   user 消息但继续运行 agent loop，见 `run_session_loop` 的 keepgoing 分支）
/// - 不发送 LocalUserBubble——TUI 不应显示空气泡
/// - 不进输入历史
async fn handle_keepgoing_submit(
    acp_client: &AcpTuiClient,
    _cwd: &str,
    admission: Option<&SubmitAdmission>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if admission.is_some_and(|admission| admission_invalidated(acp_client, admission)) {
        notify_cancelled_submit();
        return Ok(());
    }
    let adopted = admission
        .and_then(|admission| admission.cohort.as_ref())
        .and_then(CohortTicket::adopted);
    let expected = admission
        .and_then(|admission| admission.binding.clone())
        .or(adopted)
        .or_else(|| acp_client.stable_session_identity());
    let Some(identity) = expected else {
        // 无会话（或无可继续的轮次）时 no-op——按钮仅在有 summary 时显示，此为防御分支
        info!("kit submit_consumer: keepgoing ignored (no active session)");
        return Ok(());
    };
    if acp_client.stable_session_identity().as_ref() != Some(&identity) {
        if let Some(cohort) = admission.and_then(|admission| admission.cohort.as_ref()) {
            cohort.cancel();
        }
        notify_cancelled_submit();
        return Ok(());
    }

    // Issue 2026-08-05 返工：keepgoing 同样生成 request_id（每次 keepgoing RPC
    // 都是新 turn）——修复 v1 在 keepgoing 场景（无 LocalUserBubble、代际不变）
    // 下 stale 判定失效的漏洞。
    let request_id = uuid::Uuid::now_v7().to_string();

    // 与 handle_agent_text_submit 相同的 loading 状态切换：PromptSubmitted 事件
    // 由 bridge 统一管理 phase/variant/is_loading 状态。
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            child_origin: None,
            view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
            event: AcpEventData::PromptSubmitted {
                request_id: Some(request_id.clone()),
            },
            active_session_id: identity.0.clone(),
        });
    } else {
        warn!("LOCAL_EVENT_TX not initialized, PromptSubmitted event dropped");
    }
    // 递增 loading epoch：message_area 据此检测新一轮 loading 会话（同 handle_agent_text_submit）
    *LOADING_EPOCH.state().write() += 1;
    tracing::info!(
        target: "msg_scroll_diag",
        "submit_consumer: keepgoing prompt submitted, LOADING_EPOCH incremented",
    );

    if let Err(e) = acp_client
        .prompt_in_session(
            &MessageContent::text(""),
            Some(request_id.clone()),
            &identity,
        )
        .await
    {
        warn!(session_id = %identity.0, request_id = %request_id, error = ?e, "kit submit_consumer: keepgoing prompt RPC failed");
        reset_failed_prompt(&identity.0, &request_id);
        if e.code == crate::acp_client::SESSION_BINDING_CHANGED_CODE {
            if let Some(cohort) = admission.and_then(|admission| admission.cohort.as_ref()) {
                cohort.cancel();
            }
            notify_cancelled_submit();
            return Ok(());
        }
        return Err(Box::new(e));
    }
    Ok(())
}

/// 执行视图层操作。
fn execute_view_action(action: ViewActionRequest, acp_client: &AcpTuiClient, cwd: &str) {
    match action {
        ViewActionRequest::CycleProvider => {
            let current = crate::kit::atoms::SERVICE_SNAPSHOT
                .state()
                .read()
                .model_alias
                .clone();
            let aliases = ["fable", "opus", "sonnet", "haiku"];
            let index = aliases
                .iter()
                .position(|alias| *alias == current)
                .unwrap_or(0);
            let next = aliases[(index + 1) % aliases.len()];
            let client = acp_client.clone();
            tokio::spawn(async move {
                if let Err(error) = client.set_model(next).await {
                    tracing::warn!(%error, "session model switch failed");
                }
            });
        }
        ViewActionRequest::CyclePermissionMode => {
            crate::kit::permission_mode::cycle(acp_client.clone());
        }
        ViewActionRequest::ExportText(mode) => {
            let message = match export_debug_text(mode, cwd) {
                Ok(path) => i18n::tr_args(
                    "export-success",
                    &[("path".into(), FluentValue::from(path.display().to_string()))],
                ),
                Err(err) => i18n::tr_args(
                    "export-fail",
                    &[("error".into(), FluentValue::from(err.to_string()))],
                ),
            };
            *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
                message,
                until: peri_time::monotonic_now() + Duration::from_secs(5),
            });
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        }
        ViewActionRequest::Exit => {
            info!("kit submit_consumer: /exit command received, requesting app exit");
            EXIT_REQUESTED.set(true);
        }
    }
}

fn export_debug_text(
    mode: ExportMode,
    cwd: &str,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let lines = collect_debug_export_lines(mode);
    let text = lines_to_plain_text(&lines);
    let path = debug_export_path(cwd);
    std::fs::write(&path, text)?;
    Ok(path)
}

fn collect_debug_export_lines(_mode: ExportMode) -> Vec<Line<'static>> {
    let snapshot = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    for item in snapshot.items.iter() {
        let text = extract_vm_text(item);
        if !text.is_empty() {
            all_lines.push(Line::from(text));
        }
    }
    all_lines
}

fn extract_vm_text(vm: &crate::kit::tui_render_unit::TuiRenderUnit) -> String {
    use crate::kit::tui_render_unit::TuiRenderUnit;
    match vm {
        TuiRenderUnit::TuiUserBubble(data) => data.text.clone(),
        TuiRenderUnit::TuiAssistantBubble(data) => data.text.clone(),
        TuiRenderUnit::TuiToolCard(data) => format!(
            "[{}] {} -> {}",
            data.tool_name, data.input_summary, data.output_summary
        ),
        TuiRenderUnit::TuiSystemNote(data) => data.text.clone(),
        TuiRenderUnit::TuiSystemReminder(data) => data.body.clone(),
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            let mut text = format!("[SubAgent: {}]", data.agent_name);
            for child in data.view_models.iter() {
                let child_text = extract_vm_text(child);
                if !child_text.is_empty() {
                    text.push_str("\n  ");
                    text.push_str(&child_text);
                }
            }
            text
        }
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            format!("[Collapsed: {} ({} items)]", data.title, data.count)
        }
        TuiRenderUnit::TuiDivider(data) => data.label.as_deref().unwrap_or("---").to_string(),
        TuiRenderUnit::TuiTodoSummary(data) => data.text.clone(),
        TuiRenderUnit::TuiAskUserBlock(data) => data
            .items
            .iter()
            .map(|i| format!("Q: {} A: {}", i.header, i.answer))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn lines_to_plain_text(lines: &[Line<'static>]) -> String {
    let mut output = String::new();
    for line in lines {
        for span in &line.spans {
            output.push_str(span.content.as_ref());
        }
        output.push('\n');
    }
    output
}

fn debug_export_path(cwd: &str) -> PathBuf {
    Path::new(cwd).join(format!(
        "peri-debug-export-{}.txt",
        chrono::DateTime::<Local>::from(peri_time::now_wall()).format("%Y%m%d-%H%M%S")
    ))
}

/// `/clear` 失败时清空 loading 状态，防止旧会话 spinner 永久卡死。
///
/// S4.2 双保险（Issue 2026-08-05）：
/// 1. 直接写 ACP_STATE.is_loading=false——bridge 已退出（shutdown 路径）时
///    事件无人消费，直接写是唯一生效路径；
/// 2. 注入 LocalLoadingReset 内部事件——bridge 存活时同步复位 phase（幂等），
///    否则后续任意事件触发 push_acp_state 会用 phase 重算 is_loading=true，
///    造成 loading 闪回 + 提交判定竞态（误入 INPUT_BUFFER）。
fn clear_loading_state() {
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = false;
    }
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            child_origin: None,
            view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
            event: AcpEventData::LocalLoadingReset,
            active_session_id: String::new(),
        });
    } else {
        warn!("LOCAL_EVENT_TX not initialized, LocalLoadingReset event dropped");
    }
}

fn reset_failed_prompt(session_id: &str, request_id: &str) {
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            child_origin: None,
            view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
            event: AcpEventData::PromptFailed {
                request_id: request_id.to_owned(),
            },
            active_session_id: session_id.to_owned(),
        });
    } else {
        warn!(
            session_id,
            request_id, "LOCAL_EVENT_TX not initialized, prompt failure reset dropped"
        );
    }
}

/// 启动取消消费者后台任务。
///
/// 监听 CANCEL_TX channel，收到信号时调用 `acp_client.cancel()` 中断当前 agent。
/// 与 submit_consumer / notifier / bridge 并行运行，通过独立 channel 解耦。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient
/// - `rx`：CANCEL_TX 的接收端（由 entry::run_kit_fullscreen 创建 channel 时拿到）
/// - `shutdown`：与 notifier / bridge 共享的同一 CancellationToken
pub fn spawn_cancel_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<()>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit cancel_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit cancel_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit cancel_consumer: CANCEL_TX dropped, exiting");
                            break;
                        }
                        Some(()) => {
                            let pending = acp_client.cancel();
                            tokio::pin!(pending);
                            let cancel_result = peri_time::timeout(
                                Duration::from_secs(CANCEL_RPC_TIMEOUT_SECS),
                                pending.as_mut(),
                            )
                            .await;
                            let outcome = match cancel_result {
                                Ok(result) => result,
                                Err(_) => {
                                    tracing::warn!("cancel_consumer: cancellation notification pending");
                                    pending.await
                                }
                            };
                            match outcome {
                                Ok(()) => tracing::info!("cancel_consumer: cancellation notification sent"),
                                Err(error) => tracing::warn!(%error, data = ?error.data,
                                    "cancel_consumer: stop outcome unresolved"),
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 测试用可控断点：在身份落定与准入校验之间暂停指定会话的提交。
///
/// 覆盖「ensure 选 A 之后、发送之前切换/重载」的窗口；生产构建不编译。
#[cfg(test)]
mod admission_probe {
    use std::sync::Mutex;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};

    static PROBE: Mutex<Option<(String, mpsc::UnboundedSender<oneshot::Sender<()>>)>> =
        Mutex::new(None);

    /// 每条提交完成分道后通告一次，供时序回归确认 consumer 已接收。
    static REGISTRATIONS: Mutex<Option<mpsc::UnboundedSender<(String, super::LaneKey)>>> =
        Mutex::new(None);

    pub(super) fn install(session_id: &str, tx: mpsc::UnboundedSender<oneshot::Sender<()>>) {
        *PROBE.lock().unwrap() = Some((session_id.to_string(), tx));
    }

    pub(super) fn clear() {
        *PROBE.lock().unwrap() = None;
    }

    pub(super) fn install_registration_probe(tx: mpsc::UnboundedSender<(String, super::LaneKey)>) {
        *REGISTRATIONS.lock().unwrap() = Some(tx);
    }

    pub(super) fn clear_registration_probe() {
        *REGISTRATIONS.lock().unwrap() = None;
    }

    pub(super) fn note_registration(lane: &super::LaneKey, label: String) {
        let probe = REGISTRATIONS.lock().unwrap();
        if let Some(tx) = probe.as_ref() {
            let _ = tx.send((label, lane.clone()));
        }
    }

    pub(super) async fn pause(identity: &super::SessionIdentity) {
        let probe = PROBE.lock().unwrap().clone();
        let Some((session_id, tx)) = probe else {
            return;
        };
        if session_id != identity.0 {
            return;
        }
        let (release, wait) = oneshot::channel();
        if tx.send(release).is_ok() {
            // 测试挂死比断言失败更难排查：断点自带上限。
            let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
        }
    }
}

#[cfg(test)]
#[path = "submit_consumer_test.rs"]
mod tests;
