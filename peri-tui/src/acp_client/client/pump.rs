//! Ordered transport reception, wire decoding, and reverse-request admission.

use std::sync::Arc;

use peri_acp::event::AcpEvent;
use peri_acp::transport::{
    AcpTransport,
    mpsc::MpscClientTransport,
    types::{IncomingMessage, RequestId},
};
use peri_acp_types::event_data::PredictionAction;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::super::interaction_lifecycle::{
    ClaimCause, ClaimedInteraction, InteractionExpiryReason, InteractionLifecycle,
    InteractionOwner, InteractionUiOutcome, OrdinaryDecision, RegisterDecision,
    ReverseInteractionKind,
};
use super::super::interaction_response::{
    elicitation_cancel_response, permission_cancelled_response,
};
use super::super::interaction_settlement::expiry_for_cause;
use super::{AcpNotification, AcpTuiClient};

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

pub(super) fn plan_reverse_request(
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

impl AcpTuiClient {
    /// Spawn the notification pump as a tokio task. Consumes the notification
    /// sender and clones of transport/session state.
    ///
    /// `notification_tx` 由 pump task 独占持有：禁止克隆到 struct/全局/任何
    /// 长生命周期对象，否则 channel 不再随 pump 退出关闭，notifier 的
    /// recv-None 兜底失效（Issue 2）。从 client 主动发通知走显式参数传递。
    pub fn spawn_pump(&self, notification_tx: mpsc::UnboundedSender<AcpNotification>) {
        let transport = self.transport.clone();
        let lifecycle = self.lifecycle.clone();
        let user_input_queue = self.user_input_queue.clone();
        *self.notification_weak.lock().unwrap() = Some(notification_tx.downgrade());
        tokio::spawn(async move {
            Self::run_pump(transport, notification_tx, lifecycle, user_input_queue).await;
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
        user_input_queue: Arc<std::sync::atomic::AtomicBool>,
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
                                if let AcpEvent::UserInputDelivered { generation, .. } = &event {
                                    let _gate = lifecycle.operation_gate().lock().await;
                                    if user_input_queue.load(std::sync::atomic::Ordering::Acquire)
                                        && lifecycle
                                            .matches_user_input_generation(&session_id, generation)
                                    {
                                        let _ = notification_tx.send(AcpNotification::AgentEvent {
                                            session_id,
                                            event,
                                        });
                                    }
                                    continue;
                                }
                                if let AcpEvent::UserInputRunStarted {
                                    generation,
                                    request_id,
                                } = &event
                                {
                                    if !user_input_queue.load(std::sync::atomic::Ordering::Acquire)
                                    {
                                        continue;
                                    }
                                    let _gate = lifecycle.operation_gate().lock().await;
                                    let Some(claims) = lifecycle.open_user_input_run(
                                        &session_id,
                                        generation,
                                        request_id,
                                    ) else {
                                        continue;
                                    };
                                    Self::settle_claims(&transport, &notification_tx, claims).await;
                                    // Publish while holding the session gate so a snapshot
                                    // cannot project a newer run before this start.
                                    Self::deliver_ordinary(
                                        &lifecycle,
                                        &notification_tx,
                                        session_id.clone(),
                                        AcpNotification::AgentEvent { session_id, event },
                                    );
                                    continue;
                                }
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
                        {
                            let _gate = lifecycle.operation_gate().lock().await;
                            if !lifecycle
                                .should_forward_prompt_terminal(&session_id, request_id.as_deref())
                            {
                                continue;
                            }
                            let claims = lifecycle
                                .close_prompt_by_wire_identity(&session_id, request_id.as_deref());
                            Self::settle_claims(&transport, &notification_tx, claims).await;
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
                        }
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
                    let _gate = if user_input_queue.load(std::sync::atomic::Ordering::Acquire) {
                        Some(lifecycle.operation_gate().lock().await)
                    } else {
                        None
                    };
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
                                reason: InteractionExpiryReason::TransportTerminal,
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
                        reason: InteractionExpiryReason::ResponseTransportFailed,
                    }
                }
            };
            let _ = notification_tx.send(AcpNotification::InteractionTerminal {
                owner: claim.owner,
                outcome,
            });
        }
    }

    pub(super) fn flush_buffered(&self, notifications: Vec<AcpNotification>) {
        let weak = self.notification_weak.lock().unwrap().clone();
        if let Some(weak) = weak
            && let Some(tx) = weak.upgrade()
        {
            for notification in notifications {
                let _ = tx.send(notification);
            }
        }
    }
}

#[cfg(test)]
#[path = "user_input_run_test.rs"]
mod user_input_run_tests;
