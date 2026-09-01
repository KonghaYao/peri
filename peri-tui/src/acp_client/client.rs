//! Thin TUI-side wrapper around [`peri_acp::transport::mpsc::MpscClientTransport`].
//!
//! Translates raw [`IncomingMessage`]s into [`AcpNotification`]s for the TUI event
//! loop to consume. The notification pump runs as a background tokio task.

use std::sync::{Arc, Mutex};

use peri_acp::event::AcpEvent;
use peri_acp::transport::{
    AcpTransport,
    mpsc::MpscClientTransport,
    types::{AcpError, IncomingMessage, RequestId},
};
use peri_acp_types::PeriCaps;
use peri_acp_types::command::command_route::UiCommandSpec;
use peri_acp_types::event_data::PredictionAction;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, warn};

use super::interaction_lifecycle::{
    ClaimCause, ClaimedInteraction, InteractionLifecycle, InteractionOwner, InteractionUiOutcome,
    OrdinaryDecision, PromptLease, RegisterDecision, ReverseInteractionKind, TransitionKind,
};
use super::interaction_response::{elicitation_cancel_response, permission_cancelled_response};
use super::interaction_settlement::{expiry_for_cause, spawn_settlement_worker};

/// Notification events dispatched from the background pump to the TUI event loop.
#[derive(Debug)]
pub enum AcpNotification {
    /// A `notifications/agent_event` notification carrying an AcpEvent DTO.
    /// The TUI converts this to its own AgentEvent via `map_acp_event`.
    AgentEvent { session_id: String, event: AcpEvent },
    /// A `notifications/session_update` notification from the ACP server.
    SessionUpdate { session_id: String, params: Value },
    /// A `RequestPermission` request requiring HITL interaction.
    RequestPermission {
        owner: InteractionOwner,
        request_id_json: String,
        params: Value,
    },
    /// An `elicitation/create` request requiring AskUser interaction.
    Elicitation {
        owner: InteractionOwner,
        request_id_json: String,
        params: Value,
    },
    /// Local, owner-qualified terminalization of a previously published interaction.
    InteractionTerminal {
        owner: InteractionOwner,
        outcome: InteractionUiOutcome,
    },
    /// An unrecognized notification or request.
    Other { msg: String },
    /// Agent execution completed (synthetic notification from ACP server).
    /// `request_id` 为被结束 turn 的 prompt requestId（服务器回带，可选）——
    /// TUI 用它识别事件所属 turn（Issue 2026-08-05 stale 判定）。
    AgentDone {
        session_id: String,
        stop_reason: String,
        request_id: Option<String>,
    },
    /// Prediction fork 完成后的建议文本与结构化动作。
    PredictionReady {
        session_id: String,
        text: String,
        actions: Vec<PredictionAction>,
    },
    /// A `notifications/peri/*` custom notification (SubAgent, Compact, LSP, etc.)
    Peri {
        session_id: String,
        method: String,
        params: Value,
    },
    /// A `peri/unstable_event` notification carrying v2 state machine events
    /// (text-chunk, tool-started, view-commit, turn-done, etc.).
    UnstableEvent {
        session_id: String,
        event: String,
        data: Value,
    },
}

impl ReverseInteractionKind {
    fn from_method(method: &str) -> Option<Self> {
        match method {
            "session/request_permission" => Some(Self::Permission),
            "elicitation/create" => Some(Self::Elicitation),
            _ => None,
        }
    }

    fn notification(
        self,
        owner: InteractionOwner,
        request_id_json: String,
        params: Value,
    ) -> AcpNotification {
        match self {
            Self::Permission => AcpNotification::RequestPermission {
                owner,
                request_id_json,
                params,
            },
            Self::Elicitation => AcpNotification::Elicitation {
                owner,
                request_id_json,
                params,
            },
        }
    }

    fn cancellation_response(self) -> Value {
        match self {
            Self::Permission => permission_cancelled_response(),
            Self::Elicitation => elicitation_cancel_response(),
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Permission => "session/request_permission",
            Self::Elicitation => "elicitation/create",
        }
    }
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn permission_session_id(params: &Value) -> Option<&str> {
    match (params.get("sessionId"), params.get("session_id")) {
        (Some(camel), Some(snake)) => {
            let camel = nonempty_string(Some(camel))?;
            let snake = nonempty_string(Some(snake))?;
            (camel == snake).then_some(camel)
        }
        (Some(camel), None) => nonempty_string(Some(camel)),
        (None, Some(snake)) => nonempty_string(Some(snake)),
        (None, None) => None,
    }
}

fn elicitation_session_id(params: &Value) -> Option<&str> {
    nonempty_string(params.get("sessionId"))
}

fn plan_reverse_request(
    method: &str,
    id: RequestId,
    params: Value,
    lifecycle: &InteractionLifecycle,
) -> Option<RegisterDecision> {
    let kind = ReverseInteractionKind::from_method(method)?;
    let session_id = match kind {
        ReverseInteractionKind::Permission => permission_session_id(&params),
        ReverseInteractionKind::Elicitation => elicitation_session_id(&params),
    }
    .map(str::to_string);
    Some(lifecycle.register_reverse(kind, id, session_id.as_deref(), params))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProjectionMode {
    Interactive,
    Headless,
}

struct StartupRestoreGuard<'a>(&'a watch::Sender<bool>);

impl Drop for StartupRestoreGuard<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

struct SessionLoadReservationState {
    pending: Mutex<usize>,
    epoch_tx: watch::Sender<u64>,
}

/// Synchronous queue-side ownership for an ordinary session load.
///
/// The reservation is acquired before a load request enters its async consumer.
/// Dropping the guard releases exactly one queued/in-flight load and wakes prompt
/// waiters. It is deliberately not Clone: the pending count represents queue
/// ownership, not arbitrary client clones.
pub(crate) struct SessionLoadReservation {
    state: Arc<SessionLoadReservationState>,
}

impl Drop for SessionLoadReservation {
    fn drop(&mut self) {
        let mut pending = self.state.pending.lock().unwrap();
        debug_assert!(*pending > 0, "session load reservation underflow");
        *pending = pending.saturating_sub(1);
        drop(pending);
        self.state
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

/// TUI-side client that owns the ACP transport and routes notifications.
///
/// Uses one mutex-protected routing state so current/deleted decisions are
/// observed atomically by clones and by the notification pump.
///
/// `notification_tx` 刻意不存于此 struct：sender 必须由 pump task 独占持有，
/// pump 退出时 channel 关闭，notifier 的 recv-None 分支才能触发（Issue 2
/// 死代码重接）。若未来需要从 client 主动发通知，走显式参数传递，勿加回字段。
#[derive(Clone)]
pub struct AcpTuiClient {
    transport: Arc<MpscClientTransport>,
    lifecycle: InteractionLifecycle,
    projection_mode: ClientProjectionMode,
    notification_weak: Arc<Mutex<Option<mpsc::WeakUnboundedSender<AcpNotification>>>>,
    /// Startup restore is reserved before submit consumers become reachable.
    /// `ensure_session` observes this flag under the same operation gate used by
    /// new/load and waits for the reserved load to settle instead of competing
    /// with it.
    startup_restore_tx: watch::Sender<bool>,
    session_load_reservations: Arc<SessionLoadReservationState>,
    #[cfg(test)]
    transition_commit_hook:
        Arc<Mutex<Option<mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>>>>,
}

impl AcpTuiClient {
    /// Check whether a session has been created.
    pub fn has_session(&self) -> bool {
        self.lifecycle.has_session()
    }

    /// Get the current session ID, if any.
    pub fn current_session_id(&self) -> Option<String> {
        self.lifecycle.current_session_id()
    }

    /// Send a raw ACP request and return the response.
    /// Used for custom RPC methods like `workflow/list_runs`.
    pub async fn send_raw_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.transport.send_request(method, params).await
    }

    /// Create a new client wrapping an existing `MpscClientTransport`.
    ///
    /// Returns `(Self, notification_sender, notification_receiver)`. The caller must:
    /// 1. Move `notification_sender` into [`AcpTuiClient::spawn_pump`] — the pump
    ///    task must remain its **sole** holder; when the pump exits (transport
    ///    closed) the sender drops, the channel closes, and the notifier's
    ///    recv-None fallback fires (Issue 2).
    /// 2. Move `notification_receiver` to the TUI event loop (`spawn_kit_notifier`).
    pub fn new(
        transport: MpscClientTransport,
    ) -> (
        Self,
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpNotification>,
    ) {
        Self::new_with_mode(transport, ClientProjectionMode::Headless)
    }

    pub fn new_interactive(
        transport: MpscClientTransport,
    ) -> (
        Self,
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpNotification>,
    ) {
        Self::new_with_mode(transport, ClientProjectionMode::Interactive)
    }

    fn new_with_mode(
        transport: MpscClientTransport,
        projection_mode: ClientProjectionMode,
    ) -> (
        Self,
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpNotification>,
    ) {
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let transport = Arc::new(transport);
        let lifecycle = InteractionLifecycle::new();
        let notification_weak = Arc::new(Mutex::new(None));
        let (startup_restore_tx, _startup_restore_rx) = watch::channel(false);
        let (load_epoch_tx, _load_epoch_rx) = watch::channel(0_u64);
        let (settlement_tx, settlement_rx) = mpsc::unbounded_channel();
        lifecycle.install_drop_settlement_sender(settlement_tx);
        spawn_settlement_worker(
            Arc::downgrade(&transport),
            settlement_rx,
            notification_weak.clone(),
        );
        let client = Self {
            transport,
            lifecycle,
            projection_mode,
            notification_weak,
            startup_restore_tx,
            session_load_reservations: Arc::new(SessionLoadReservationState {
                pending: Mutex::new(0),
                epoch_tx: load_epoch_tx,
            }),
            #[cfg(test)]
            transition_commit_hook: Arc::new(Mutex::new(None)),
        };
        (client, notification_tx, notification_rx)
    }

    /// Spawn the notification pump as a tokio task. Consumes the notification
    /// sender and clones of transport/session state.
    ///
    /// `notification_tx` 由 pump task 独占持有：禁止克隆到 struct/全局/任何
    /// 长生命周期对象，否则 channel 不再随 pump 退出关闭，notifier 的
    /// recv-None 兜底失效（Issue 2）。从 client 主动发通知走显式参数传递。
    pub fn spawn_pump(&self, notification_tx: mpsc::UnboundedSender<AcpNotification>) {
        let transport = self.transport.clone();
        let lifecycle = self.lifecycle.clone();
        *self.notification_weak.lock().unwrap() = Some(notification_tx.downgrade());
        tokio::spawn(async move {
            Self::run_pump(transport, notification_tx, lifecycle).await;
        });
    }

    /// 检查 session_id 是否匹配当前会话。
    ///
    /// 当 `current_session_id` 为 `None`（首次连接、尚未创建会话）时返回 `true`，
    /// 确保 `AvailableCommandsUpdate` 等初始化通知不被丢弃。
    /// 当已设置会话后，严格按 session_id 过滤。
    /// 已删除会话（黑名单）一律返回 `false`——优先级高于 None 放行语义（M3）。
    fn deliver_ordinary(
        lifecycle: &InteractionLifecycle,
        notification_tx: &mpsc::UnboundedSender<AcpNotification>,
        session_id: String,
        notification: AcpNotification,
    ) {
        match lifecycle.route_ordinary(session_id, notification) {
            OrdinaryDecision::Forward(notification) => {
                let _ = notification_tx.send(*notification);
            }
            OrdinaryDecision::Buffered | OrdinaryDecision::Drop => {}
        }
    }

    // ── Pump ──

    /// Background task that polls the transport and dispatches notifications.
    async fn run_pump(
        transport: Arc<MpscClientTransport>,
        notification_tx: mpsc::UnboundedSender<AcpNotification>,
        lifecycle: InteractionLifecycle,
    ) {
        let mut event_count: u64 = 0;
        loop {
            let msg = transport.recv().await;
            match msg {
                Some(IncomingMessage::Notification { method, params }) => {
                    if method == "peri/agent_event" {
                        event_count += 1;
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // Prefer pre-serialized string (avoids clone + double-deserialize).
                        // Fall back to old "event" Value field for backward compat during rollout.
                        let event_result = if let Some(event_str) =
                            params.get("event_json").and_then(|v| v.as_str())
                        {
                            serde_json::from_str::<AcpEvent>(event_str)
                        } else if let Some(event_value) = params.get("event") {
                            serde_json::from_value::<AcpEvent>(event_value.clone())
                        } else {
                            warn!(
                                "ACP client pump: agent_event notification missing 'event_json' or 'event' field"
                            );
                            continue;
                        };
                        match event_result {
                            Ok(event) => {
                                debug!(
                                    event_count = event_count,
                                    session_id = %session_id,
                                    "ACP client pump: received agent_event"
                                );
                                Self::deliver_ordinary(
                                    &lifecycle,
                                    &notification_tx,
                                    session_id.clone(),
                                    AcpNotification::AgentEvent { session_id, event },
                                );
                            }
                            Err(e) => {
                                error!(
                                    event_count = event_count,
                                    error = %e,
                                    "ACP client pump: failed to parse AcpEvent — event LOST"
                                );
                                let _ = notification_tx.send(AcpNotification::Other {
                                    msg: format!("failed to parse AcpEvent: {e}"),
                                });
                            }
                        }
                    } else if method == "peri/agent_activity" {
                        // Compact GUI projection. The TUI already owns richer
                        // native event rendering, so consume this negotiated
                        // duplicate without forwarding it into the UI loop.
                        debug!("ACP client pump: ignoring duplicate agent activity projection");
                    } else if method == "session/update" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        Self::deliver_ordinary(
                            &lifecycle,
                            &notification_tx,
                            session_id.clone(),
                            AcpNotification::SessionUpdate { session_id, params },
                        );
                    } else if method == "peri/unstable_event" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let event = params
                            .get("event")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let data = params.get("data").cloned().unwrap_or(Value::Null);
                        debug!(
                            session_id = %session_id,
                            event = %event,
                            "ACP client pump: received unstable_event"
                        );
                        Self::deliver_ordinary(
                            &lifecycle,
                            &notification_tx,
                            session_id.clone(),
                            AcpNotification::UnstableEvent {
                                session_id,
                                event,
                                data,
                            },
                        );
                    } else if method == "peri/agent_event_done" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        debug!(
                            session_id = %session_id,
                            total_events = event_count,
                            "ACP client pump: received agent_event_done"
                        );
                        let stop_reason = params
                            .get("stopReason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("end_turn")
                            .to_string();
                        // requestId 为可选字段（缺失路径如 continuation/Immediate 命令/stdio）
                        let request_id = params
                            .get("requestId")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        if lifecycle.is_current_session(&session_id) {
                            let _gate = lifecycle.operation_gate().lock().await;
                            let claims = lifecycle
                                .close_prompt_by_wire_identity(&session_id, request_id.as_deref());
                            Self::settle_claims(&transport, &notification_tx, claims).await;
                        }
                        Self::deliver_ordinary(
                            &lifecycle,
                            &notification_tx,
                            session_id.clone(),
                            AcpNotification::AgentDone {
                                session_id,
                                stop_reason,
                                request_id,
                            },
                        );
                    } else if method == "peri/prediction_ready" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = params
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let actions = params
                            .get("actions")
                            .and_then(|v| {
                                serde_json::from_value::<Vec<PredictionAction>>(v.clone()).ok()
                            })
                            .unwrap_or_default();
                        if !actions.is_empty() || !text.is_empty() {
                            Self::deliver_ordinary(
                                &lifecycle,
                                &notification_tx,
                                session_id.clone(),
                                AcpNotification::PredictionReady {
                                    session_id,
                                    text,
                                    actions,
                                },
                            );
                        }
                    } else if method.starts_with("notifications/peri/") {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        Self::deliver_ordinary(
                            &lifecycle,
                            &notification_tx,
                            session_id.clone(),
                            AcpNotification::Peri {
                                session_id,
                                method,
                                params,
                            },
                        );
                    } else {
                        let _ = notification_tx.send(AcpNotification::Other {
                            msg: format!("notification: {method}"),
                        });
                    }
                }
                Some(IncomingMessage::Request { id, method, params }) => {
                    match plan_reverse_request(&method, id, params, &lifecycle) {
                        Some(RegisterDecision::Settle { kind, id }) => {
                            Self::settle_reverse_request(&transport, kind, id).await;
                        }
                        Some(RegisterDecision::Forward(registered)) => {
                            let owner = registered.owner.clone();
                            let kind = owner.kind;
                            if notification_tx
                                .send(kind.notification(
                                    registered.owner,
                                    registered.request_id_json,
                                    registered.params,
                                ))
                                .is_err()
                                && let Some(claimed) =
                                    lifecycle.claim(&owner, ClaimCause::BridgeReject)
                            {
                                Self::settle_claims(&transport, &notification_tx, vec![claimed])
                                    .await;
                            }
                        }
                        None => {
                            let _ = notification_tx.send(AcpNotification::Other {
                                msg: format!("request: {method}"),
                            });
                        }
                    }
                }
                Some(IncomingMessage::Response { .. }) => {}
                None => {
                    debug!("ACP client pump: transport closed, exiting");
                    let _operation = lifecycle.operation_gate().lock().await;
                    let claims = lifecycle.transport_terminal();
                    for claim in claims {
                        let _ = notification_tx.send(AcpNotification::InteractionTerminal {
                            owner: claim.owner,
                            outcome: InteractionUiOutcome::Expired {
                                reason: super::interaction_lifecycle::InteractionExpiryReason::TransportTerminal,
                            },
                        });
                    }
                    break;
                }
            }
        }
    }

    async fn settle_reverse_request(
        transport: &MpscClientTransport,
        kind: ReverseInteractionKind,
        id: RequestId,
    ) {
        if let Err(error) = transport
            .send_response(id, Ok(kind.cancellation_response()))
            .await
        {
            warn!(
                method = kind.method(),
                error = %error,
                "ACP client pump: failed to settle reverse request"
            );
        }
    }

    async fn settle_claims(
        transport: &MpscClientTransport,
        notification_tx: &mpsc::UnboundedSender<AcpNotification>,
        claims: Vec<ClaimedInteraction>,
    ) {
        for claim in claims {
            let response = match claim.owner.kind {
                ReverseInteractionKind::Permission => permission_cancelled_response(),
                ReverseInteractionKind::Elicitation => elicitation_cancel_response(),
            };
            let outcome = match transport
                .send_response(claim.request_id, Ok(response))
                .await
            {
                Ok(()) => InteractionUiOutcome::Expired {
                    reason: expiry_for_cause(claim.cause),
                },
                Err(error) => {
                    warn!(error = %error, "failed to settle claimed reverse interaction");
                    InteractionUiOutcome::Expired {
                        reason: super::interaction_lifecycle::InteractionExpiryReason::ResponseTransportFailed,
                    }
                }
            };
            let _ = notification_tx.send(AcpNotification::InteractionTerminal {
                owner: claim.owner,
                outcome,
            });
        }
    }

    // ── High-level RPC wrappers ──

    /// 上送 ui 域命令明细（设计 §88 / Phase 3 caps 通道：`clientCapabilities._meta.
    /// peri.uiCommands` 明细数组）。
    ///
    /// TUI 内部 Mpsc 路径无协议 initialize 握手，host 默认以
    /// [`PeriCaps::all_enabled`] 兜底（含 11 条旧兜底 ui 明细）。本方法显式协商
    /// caps：以 all_enabled 为基座、替换 `ui_commands` 为 TUI 实时明细，经
    /// initialize 请求送达 host（`handle_request` "initialize" 分支 → `set_pending_caps`
    /// → 首个 session/new 的 `ensure_session_caps` 取协商值 → `send_available_commands_update`
    /// 把明细注册进注册表，投影回推刷新补全缓存）。
    ///
    /// **时序契约**：必须在首个 `session/new` 之前调用（initialize 是进程级
    /// 一次性协商）；失败仅 warn 不阻断——host 回退 all_enabled 兜底明细，
    /// 首 session 仍可用（R2 双写窗口防御）。
    pub async fn register_ui_commands(&self, specs: &[UiCommandSpec]) -> Result<(), AcpError> {
        let mut caps = PeriCaps::all_enabled();
        caps.ui_commands = specs.to_vec();
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "_meta": caps.to_agent_meta() },
        });
        self.transport.send_request("initialize", params).await?;
        Ok(())
    }

    /// Create a new agent session.
    ///
    /// Closes the previous session (if any) to release its history, AgentPool,
    /// and FrozenSessionData from the server-side sessions HashMap.
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.new_session_under_gate(cwd, model).await
    }

    async fn new_session_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::New, None)
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(None);
        }
        self.settle_claims_owned(start.claims).await;
        let old_id = start.from;
        if let Some(ref old_sid) = old_id {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "cwd": cwd, "model": model });
        let result = match self.transport.send_request("session/new", params).await {
            Ok(result) => result,
            Err(error) => {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                return Err(error);
            }
        };
        // ACP protocol uses camelCase: {"sessionId": "..."}
        let session_id = result
            .get("sessionId")
            .or_else(|| result.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| AcpError::new(-32603, "no session_id in response"))?
            .to_string();
        #[cfg(test)]
        self.pause_before_transition_commit().await;
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(&session_id));
        }
        let buffered = self
            .lifecycle
            .commit_stable(start.generation, session_id.clone());
        transition.disarm();
        self.flush_buffered(buffered);
        Ok(session_id)
    }

    #[cfg(test)]
    fn install_transition_commit_hook(
        &self,
        tx: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    ) {
        *self.transition_commit_hook.lock().unwrap() = Some(tx);
    }

    #[cfg(test)]
    async fn pause_before_transition_commit(&self) {
        let tx = self.transition_commit_hook.lock().unwrap().clone();
        if let Some(tx) = tx {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            if tx.send(release_tx).is_ok() {
                let _ = release_rx.await;
            }
        }
    }

    /// Return the current stable session, creating it only if no startup restore
    /// owns the lifecycle decision.  Both the stable re-check and a possible
    /// `session/new` execute under the client operation gate.
    pub async fn ensure_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        let mut startup_restore_rx = self.startup_restore_tx.subscribe();
        let mut load_epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let operation = self.lifecycle.operation_gate().lock().await;
            let (pending_load, stable_session) = {
                let _ = *load_epoch_rx.borrow_and_update();
                let pending = self.session_load_reservations.pending.lock().unwrap();
                if *pending > 0 {
                    (true, None)
                } else {
                    // Hold the reservation mutex through Stable selection so a
                    // dispatcher linearizes strictly before or after this result.
                    (
                        false,
                        self.lifecycle
                            .stable_identity()
                            .map(|(session_id, _)| session_id),
                    )
                }
            };
            if pending_load {
                drop(operation);
                self.wait_for_session_load(&mut load_epoch_rx).await?;
                continue;
            }
            if let Some(session_id) = stable_session {
                return Ok(session_id);
            }
            if *startup_restore_rx.borrow_and_update() {
                drop(operation);
                startup_restore_rx.changed().await.map_err(|_| {
                    AcpError::new(-32603, "startup restore reservation closed unexpectedly")
                })?;
                continue;
            }
            return self.new_session_under_gate(cwd, model).await;
        }
    }

    /// Reserve an ordinary session load before handing it to an async consumer.
    /// This synchronous boundary closes the browser-select → consumer scheduling
    /// window where a fast submit could otherwise bind to the old Stable session.
    pub(crate) fn reserve_session_load(&self) -> SessionLoadReservation {
        let mut pending = self.session_load_reservations.pending.lock().unwrap();
        *pending = pending
            .checked_add(1)
            .expect("session load reservation count overflow");
        drop(pending);
        self.session_load_reservations
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        SessionLoadReservation {
            state: Arc::clone(&self.session_load_reservations),
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_session_load_count(&self) -> usize {
        *self.session_load_reservations.pending.lock().unwrap()
    }

    async fn wait_for_session_load(
        &self,
        epoch_rx: &mut watch::Receiver<u64>,
    ) -> Result<(), AcpError> {
        loop {
            let _ = *epoch_rx.borrow_and_update();
            if *self.session_load_reservations.pending.lock().unwrap() == 0 {
                return Ok(());
            }
            epoch_rx.changed().await.map_err(|_| {
                AcpError::new(-32603, "session load reservation closed unexpectedly")
            })?;
        }
    }

    async fn open_prompt_after_session_loads(
        &self,
        request_id: Option<String>,
    ) -> Result<(String, PromptLease), AcpError> {
        let mut epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let opened = {
                let _operation = self.lifecycle.operation_gate().lock().await;
                let pending = self.session_load_reservations.pending.lock().unwrap();
                let _ = *epoch_rx.borrow_and_update();
                if *pending > 0 {
                    None
                } else {
                    let session_id = self
                        .lifecycle
                        .stable_identity()
                        .map(|(session_id, _)| session_id)
                        .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
                    let lease = self
                        .lifecycle
                        .open_prompt(request_id.clone())
                        .map_err(|message| AcpError::new(-32603, message))?;
                    // Keep the reservation mutex through open_prompt: a dispatcher
                    // linearizes strictly before this prompt or after it.
                    Some((session_id, lease))
                }
            };
            if let Some(opened) = opened {
                return Ok(opened);
            }
            self.wait_for_session_load(&mut epoch_rx).await?;
        }
    }

    /// Establish resume/continue ownership before submit consumers can choose a
    /// fresh session. The reservation mutation shares the lifecycle operation
    /// gate with `ensure_session`, `new_session`, and `load_session`.
    pub async fn reserve_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.startup_restore_tx.send_replace(true);
    }

    /// Resolve a startup restore reservation by loading its selected target.
    /// Waiters are released only after load commits Stable (or fails closed).
    pub async fn load_startup_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let reservation = StartupRestoreGuard(&self.startup_restore_tx);
        let result = self.load_session_under_gate(session_id, cwd, model).await;
        drop(reservation);
        result
    }

    /// Release a startup reservation when resume lookup cannot select a target.
    pub async fn release_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.startup_restore_tx.send_replace(false);
    }

    /// Load an existing session from ThreadStore history.
    /// Used when restoring a historical thread so the ACP server has the full context.
    ///
    /// Closes the previous session (if any) to release server-side memory.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.load_session_under_gate(session_id, cwd, model).await
    }

    async fn load_session_under_gate(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::Load, Some(session_id.to_string()))
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(session_id));
        }
        self.settle_claims_owned(start.claims).await;
        let old_id = start.from;
        if let Some(ref old_sid) = old_id
            && old_sid != session_id
        {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "sessionId": session_id, "cwd": cwd, "model": model });
        if let Err(error) = self.transport.send_request("session/load", params).await {
            self.lifecycle.fail_transition(start.generation);
            transition.disarm();
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            return Err(error);
        }
        self.lifecycle
            .commit_stable(start.generation, session_id.to_string());
        transition.disarm();
        Ok(session_id.to_string())
    }

    /// Delete a session from history (standard ACP `session/delete`).
    ///
    /// 遵守 agentclientprotocol.com/protocol/v1/session-delete：`{ sessionId }`
    /// 请求、`{}` 响应；删除后会话不再出现在 `session/list` 中且无法
    /// `session/load`。若删除的是当前活跃会话，本地事实源一并清空
    /// （服务端会 cancel 该会话的 in-flight turn 并级联删除消息）。
    ///
    /// M3：删除的会话 id 记入黑名单，pump 过滤其延迟通知（`current_session_id`
    /// 置 None 后"首次连接放行"语义会让已删除会话的事件回写 UI）。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let is_current = self
            .lifecycle
            .stable_identity()
            .as_ref()
            .is_some_and(|(id, _)| id == session_id);
        let transition = if is_current {
            let start = self
                .lifecycle
                .begin_transition(TransitionKind::DeleteCurrent, None)
                .map_err(|message| AcpError::new(-32603, message))?;
            let lease = self.lifecycle.arm_transition(start.generation);
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            self.settle_claims_owned(start.claims).await;
            Some((start.generation, lease))
        } else {
            None
        };
        let params = json!({ "sessionId": session_id });
        let result = self.transport.send_request("session/delete", params).await;
        match transition {
            Some((generation, lease)) => {
                self.lifecycle.fail_transition(generation);
                lease.disarm();
                if result.is_ok() {
                    self.lifecycle.mark_deleted(session_id);
                }
            }
            None if result.is_ok() => self.lifecycle.mark_deleted(session_id),
            None => {}
        }
        result.map(|_| ())
    }

    /// Submit a user message to the current session.
    /// Note: prompt() is called from the spawned async task that already
    /// has a session via new_session(), so current_session_id is guaranteed Some.
    ///
    /// `request_id` 为本轮 prompt 的唯一标识（submit_consumer 生成）——服务器
    /// 随 turn 结束事件（peri/agent_event_done）回带，供 stale 事件配对判定
    /// （Issue 2026-08-05）。None = 缺失路径（不注入 params）。
    pub async fn prompt(
        &self,
        content: &peri_acp_types::messages::MessageContent,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let (session_id, lease) = self
            .open_prompt_after_session_loads(request_id.clone())
            .await?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        let result = self.transport.send_request("session/prompt", params).await;
        let _operation = self.lifecycle.operation_gate().lock().await;
        let claims = lease.finish();
        self.settle_claims_owned(claims).await;
        result.map(|_| ())
    }

    /// Submit a user message with background task results attached.
    ///
    /// The server-side executor injects the bg_results as `Defer` messages into the
    /// v2 MessageQueue (see `peri-acp/src/session/executor.rs`). Defer is the
    /// correct semantic for async-delayed results: Receive skips them, End drains
    /// and awakens a new turn, and `run_react_loop` writes them to the transcript
    /// wrapped in `<system-reminder>` (see `append_messages_to_transcript`).
    pub async fn prompt_with_bg_results(
        &self,
        content: &peri_acp_types::messages::MessageContent,
        bg_results: Vec<peri_acp_types::event::BackgroundTaskResult>,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let (session_id, lease) = self
            .open_prompt_after_session_loads(request_id.clone())
            .await?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
            "bgResults": bg_results,
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        let result = self.transport.send_request("session/prompt", params).await;
        let _operation = self.lifecycle.operation_gate().lock().await;
        let claims = lease.finish();
        self.settle_claims_owned(claims).await;
        result.map(|_| ())
    }

    /// Change the model for the current session.
    pub async fn set_model(&self, alias: &str) -> Result<(), AcpError> {
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let params = json!({ "sessionId": session_id, "modelId": alias });
        let _ = self
            .transport
            .send_request("session/set_model", params)
            .await?;
        Ok(())
    }

    /// Change the permission mode for the current session.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AcpError> {
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let params = json!({ "sessionId": session_id, "modeId": mode });
        let _ = self
            .transport
            .send_request("session/set_mode", params)
            .await?;
        Ok(())
    }

    /// Set a config option (mode/model/thought_level) via the unified config API.
    /// Silently returns Ok if no session exists yet — uses notification to
    /// update ACP server state directly without requiring a session.
    pub async fn set_config_option(&self, config_id: &str, value: &str) -> Result<(), AcpError> {
        let session_id = self.lifecycle.current_session_id();
        match session_id {
            Some(session_id) => {
                let params =
                    json!({ "sessionId": session_id, "configId": config_id, "value": value });
                let _ = self
                    .transport
                    .send_request("session/set_config_option", params)
                    .await?;
            }
            None => {
                // No session yet — send via notification so ACP server updates its
                // peri_config/provider before any session is created.
                let params = json!({ "configId": config_id, "value": value });
                self.transport
                    .send_notification("session/config_update", params)
                    .await?;
            }
        }
        Ok(())
    }

    /// Update the full PeriConfig on the ACP server (for Login panel CRUD).
    /// When no session exists, uses notification to update server state directly.
    pub async fn update_config(&self, config: &crate::config::PeriConfig) -> Result<(), AcpError> {
        let session_id = self.lifecycle.current_session_id();
        match session_id {
            Some(session_id) => {
                let params = json!({
                    "sessionId": session_id,
                    "config": config,
                });
                let _ = self
                    .transport
                    .send_request("session/update_config", params)
                    .await?;
            }
            None => {
                // No session yet — send via notification so ACP server updates
                // peri_config/provider before any session is created.
                tracing::info!("update_config: no session, sending via notification");
                let params = json!({
                    "config": config,
                });
                self.transport
                    .send_notification("session/config_update", params)
                    .await?;
            }
        }
        Ok(())
    }

    /// Cancel the currently running prompt.
    pub async fn cancel(&self) -> Result<(), AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let claims = self.lifecycle.cancel_active_prompt();
        self.settle_claims_owned(claims).await;
        let params = json!({ "sessionId": session_id });
        self.transport
            .send_notification("session/cancel", params)
            .await
    }

    /// Cancel a specific background task by task_id.
    pub async fn cancel_bg_task(&self, session_id: &str, task_id: &str) -> Result<Value, AcpError> {
        self.send_raw_request(
            "session/cancel-bg-task",
            json!({ "sessionId": session_id, "taskId": task_id }),
        )
        .await
    }

    /// Kill a workflow run by run_id（Workflow 面板 Enter / workflow/kill_run RPC）。
    /// 与 cancel_bg_task 对 Workflow 类型任务等效：走同一 WorkflowTaskRegistry::kill 通道。
    pub async fn kill_workflow_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Value, AcpError> {
        self.send_raw_request(
            "workflow/kill_run",
            json!({ "sessionId": session_id, "runId": run_id }),
        )
        .await
    }

    /// Claim and settle an interaction. A stale owner is a successful no-op.
    pub async fn respond_interaction(
        &self,
        owner: &InteractionOwner,
        response: Value,
        result: String,
    ) -> Result<bool, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::UserResponse) else {
            return Ok(false);
        };
        let wire_result = self
            .transport
            .send_response(claimed.request_id, Ok(response))
            .await;
        let outcome = match &wire_result {
            Ok(()) => InteractionUiOutcome::Resolved { result },
            Err(_) => InteractionUiOutcome::Expired {
                reason:
                    super::interaction_lifecycle::InteractionExpiryReason::ResponseTransportFailed,
            },
        };
        self.emit_terminal(claimed.owner, outcome);
        wire_result.map(|_| true)
    }

    /// Ordered bridge publication seam. The gate remains held until all atom /
    /// popup / panel / durable-block publication in `publish` has completed.
    pub async fn publish_if_owned(&self, owner: &InteractionOwner, publish: impl FnOnce()) -> bool {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let projection_matches = self.projection_mode == ClientProjectionMode::Interactive
            && crate::kit::atoms::ACTIVE_SESSION_ID.state().read().as_str() == owner.session_id;
        if self.lifecycle.is_pending_owner(owner) && projection_matches {
            publish();
            return true;
        }
        if let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::BridgeReject) {
            self.settle_claims_owned(vec![claimed]).await;
        }
        false
    }

    pub async fn reject_interaction(&self, owner: &InteractionOwner) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        if let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::BridgeReject) {
            self.settle_claims_owned(vec![claimed]).await;
        }
    }

    fn flush_buffered(&self, notifications: Vec<AcpNotification>) {
        let weak = self.notification_weak.lock().unwrap().clone();
        if let Some(weak) = weak
            && let Some(tx) = weak.upgrade()
        {
            for notification in notifications {
                let _ = tx.send(notification);
            }
        }
    }

    fn emit_terminal(&self, owner: InteractionOwner, outcome: InteractionUiOutcome) {
        let weak = self.notification_weak.lock().unwrap().clone();
        if let Some(weak) = weak
            && let Some(tx) = weak.upgrade()
        {
            let _ = tx.send(AcpNotification::InteractionTerminal { owner, outcome });
        }
    }

    async fn settle_claims_owned(&self, claims: Vec<ClaimedInteraction>) {
        let mut batch = self.lifecycle.arm_claimed_batch(claims);
        while let Some(lease) = batch.next_claim() {
            let claim = lease.claim();
            let response = match claim.owner.kind {
                ReverseInteractionKind::Permission => permission_cancelled_response(),
                ReverseInteractionKind::Elicitation => elicitation_cancel_response(),
            };
            let outcome = match self
                .transport
                .send_response(claim.request_id.clone(), Ok(response))
                .await
            {
                Ok(()) => InteractionUiOutcome::Expired {
                    reason: expiry_for_cause(claim.cause),
                },
                Err(error) => {
                    warn!(error = %error, "failed to settle owned interaction");
                    InteractionUiOutcome::Expired {
                        reason: super::interaction_lifecycle::InteractionExpiryReason::ResponseTransportFailed,
                    }
                }
            };
            let claim = lease.complete();
            self.emit_terminal(claim.owner, outcome);
        }
    }

    #[cfg(test)]
    pub(crate) fn force_stable_for_test(&self, session_id: &str, accepting_reverse: bool) {
        self.lifecycle.force_stable(session_id, accepting_reverse);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::mpsc::mpsc_transport_pair;

    /// Issue 2026-08-05 返工链路测试：pump 解析 `peri/agent_event_done` 的
    /// requestId → `AgentDone.request_id`（服务器回带 → TUI stale 配对）。
    #[tokio::test]
    async fn test_pump_parses_agent_event_done_request_id() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.lifecycle.force_stable("s1", false);
        client.spawn_pump(notification_tx);

        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({
                    "sessionId": "s1",
                    "stopReason": "cancelled",
                    "requestId": "rid-1",
                }),
            )
            .await
            .unwrap();

        match notification_rx.recv().await.unwrap() {
            AcpNotification::AgentDone {
                session_id,
                stop_reason,
                request_id,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(stop_reason, "cancelled");
                assert_eq!(request_id.as_deref(), Some("rid-1"));
            }
            other => panic!("expected AgentDone, got {other:?}"),
        }
    }

    /// 兼容性：requestId 缺失时 AgentDone.request_id 应为 None（continuation /
    /// Immediate 命令 / stdio 等路径）。
    #[tokio::test]
    async fn test_pump_agent_event_done_without_request_id() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.lifecycle.force_stable("s1", false);
        client.spawn_pump(notification_tx);

        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({ "sessionId": "s1", "stopReason": "end_turn" }),
            )
            .await
            .unwrap();

        match notification_rx.recv().await.unwrap() {
            AcpNotification::AgentDone {
                session_id,
                stop_reason,
                request_id,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(stop_reason, "end_turn");
                assert_eq!(request_id, None);
            }
            other => panic!("expected AgentDone, got {other:?}"),
        }
    }

    // ── M3 回归：已删除会话的延迟通知必须被过滤 ──────────────────────────────

    /// 复现"幽灵播放"场景：current_session_id=None（删除当前会话后）时，
    /// 黑名单中的会话事件必须被 drop——None 放行语义只服务于首次连接初始化。
    #[tokio::test]
    async fn test_pump_drops_events_from_deleted_session_when_current_none() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.spawn_pump(notification_tx);

        // 删除会话（模拟：current 置 None + 黑名单插入）
        client.lifecycle.mark_deleted("deleted-sess");

        // 已删除会话的残流通知（agent_event / unstable_event / agent_done）
        server_transport
            .send_notification(
                "peri/agent_event",
                json!({ "sessionId": "deleted-sess", "event_json": serde_json::to_string(&AcpEvent::StateSnapshot { messages_json: "[]".to_string() }).unwrap() }),
            )
            .await
            .unwrap();
        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({ "sessionId": "deleted-sess", "stopReason": "cancelled" }),
            )
            .await
            .unwrap();

        // 无事件应到达 UI
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            notification_rx.recv(),
        )
        .await
        {
            Err(_) => {} // 超时 = 全部被过滤 ✓
            Ok(Some(other)) => panic!("已删除会话的事件不应回写 UI: {other:?}"),
            Ok(None) => panic!("pump 意外退出"),
        }
    }

    /// 黑名单不误伤：current=None 时未删除会话的初始化通知仍正常放行。
    #[tokio::test]
    async fn test_pump_still_forwards_init_notifications_when_current_none() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.lifecycle.force_stable("s-init", false);
        client.spawn_pump(notification_tx);

        server_transport
            .send_notification(
                "session/update",
                json!({ "sessionId": "s-init", "commands": [] }),
            )
            .await
            .unwrap();

        match notification_rx.recv().await.unwrap() {
            AcpNotification::SessionUpdate { session_id, .. } => {
                assert_eq!(session_id, "s-init");
            }
            other => panic!("初始化通知应放行, got {other:?}"),
        }
    }

    /// delete_session 完整链路：服务端响应后，current 清空 + 黑名单记录，
    /// 该会话后续事件被过滤。
    #[tokio::test]
    async fn test_delete_session_clears_current_and_blacklists() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.spawn_pump(notification_tx);

        // 先构造"当前会话"状态（等价于 new_session 成功后）
        client.lifecycle.force_stable("sess-1", false);

        // server 端响应 session/delete（标准空对象）
        let server_transport = std::sync::Arc::new(server_transport);
        let server_tx_for_task = server_transport.clone();
        let server = tokio::spawn(async move {
            let msg = server_tx_for_task.recv().await.unwrap();
            let IncomingMessage::Request { id, method, params } = msg else {
                panic!("expected request, got {msg:?}");
            };
            assert_eq!(method, "session/delete");
            assert_eq!(
                params.get("sessionId").and_then(|v| v.as_str()),
                Some("sess-1")
            );
            server_tx_for_task
                .send_response(id, Ok(serde_json::json!({})))
                .await
                .unwrap();
        });

        client
            .delete_session("sess-1")
            .await
            .expect("delete 应成功");
        server.await.unwrap();

        assert!(
            client.current_session_id().is_none(),
            "删除当前会话后 current_session_id 应清空"
        );
        assert!(
            !client.lifecycle.is_current_session("sess-1"),
            "删除的会话应记入黑名单"
        );

        // 已删除会话的残流被过滤
        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({ "sessionId": "sess-1", "stopReason": "cancelled" }),
            )
            .await
            .unwrap();
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            notification_rx.recv(),
        )
        .await
        {
            Err(_) => {}
            Ok(Some(other)) => panic!("已删除会话的事件不应回写 UI: {other:?}"),
            Ok(None) => panic!("pump 意外退出"),
        }
    }
}

#[cfg(test)]
#[path = "client_reverse_test.rs"]
mod reverse_tests;
