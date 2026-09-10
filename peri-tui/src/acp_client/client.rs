//! Thin TUI-side wrapper around [`peri_acp::transport::mpsc::MpscClientTransport`].
//!
//! Translates raw [`peri_acp::transport::types::IncomingMessage`]s into [`AcpNotification`]s for the TUI event
//! loop to consume. The notification pump runs as a background tokio task.

use std::sync::{Arc, Mutex};

use peri_acp::event::AcpEvent;
use peri_acp::transport::mpsc::MpscClientTransport;
use peri_acp_types::event_data::PredictionAction;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use super::interaction_lifecycle::{InteractionLifecycle, InteractionOwner, InteractionUiOutcome};
use super::interaction_settlement::spawn_settlement_worker;
use session::SessionLoadReservationState;

mod interaction;
mod pump;
mod requests;
mod session;

pub(crate) use session::SessionLoadReservation;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProjectionMode {
    Interactive,
    Headless,
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

    #[cfg(test)]
    pub(crate) fn force_stable_for_test(&self, session_id: &str, accepting_reverse: bool) {
        self.lifecycle.force_stable(session_id, accepting_reverse);
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[cfg(test)]
#[path = "client_reverse_test.rs"]
mod reverse_tests;
