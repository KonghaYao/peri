//! 真实 notifier → bridge 边界：交付气泡与 assistant 输出共享同一个 FIFO。

use peri_acp::event::AcpEvent;
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_acp_types::{
    messages::MessageContent,
    session::{UserInput, UserInputQueueSnapshot, UserInputQueueTerminalItem, UserInputState},
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::acp_client::{AcpNotification, AcpTuiClient};
use crate::kit::atoms::{self, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, VIEW_MODELS};
use crate::kit::steer_state::{STEERS, SteerCommand, SteerCommandKind, SteerState};
use crate::kit::tui_render_unit::TuiRenderUnit;

fn snapshot() -> UserInputQueueSnapshot {
    UserInputQueueSnapshot {
        session_id: "s".into(),
        generation: "g".into(),
        revision: 1,
        active_request_id: None,
        items: vec![],
        terminal_items: vec![],
    }
}

struct RestoreAtoms {
    steers: SteerState,
    active: String,
    counter: u64,
    view: atoms::ViewModelsSnapshot,
}

impl RestoreAtoms {
    fn new() -> Self {
        crate::kit::atoms::init_atoms();
        let old = Self {
            steers: STEERS.state().read().clone(),
            active: ACTIVE_SESSION_ID.state().read().clone(),
            counter: BRIDGE_RESET_COUNTER.get(),
            view: VIEW_MODELS.state().read().clone(),
        };
        STEERS.set(SteerState::default());
        *ACTIVE_SESSION_ID.state().write() = "s".into();
        BRIDGE_RESET_COUNTER.set(old.counter.wrapping_add(1));
        let epoch = BRIDGE_RESET_COUNTER.get();
        {
            let atom = STEERS.state();
            let mut steers = atom.write();
            steers.reset_session("s", epoch);
            steers.accept_snapshot(snapshot(), epoch, true);
        }
        old
    }
}

impl Drop for RestoreAtoms {
    fn drop(&mut self) {
        STEERS.set(self.steers.clone());
        *ACTIVE_SESSION_ID.state().write() = self.active.clone();
        BRIDGE_RESET_COUNTER.set(self.counter);
        *VIEW_MODELS.state().write() = self.view.clone();
    }
}

fn send_delivered(tx: &mpsc::UnboundedSender<AcpNotification>, input_id: &str, text: &str) {
    tx.send(AcpNotification::AgentEvent {
        session_id: "s".into(),
        event: AcpEvent::UserInputDelivered {
            generation: "g".into(),
            input_id: input_id.into(),
            content: MessageContent::text(text),
        },
    })
    .unwrap();
}

fn send_text(tx: &mpsc::UnboundedSender<AcpNotification>, text: &str) {
    tx.send(AcpNotification::SessionUpdate {
        session_id: "s".into(),
        params: json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "messageId": "m-answer",
                "content": {"type": "text", "text": text},
            }
        }),
    })
    .unwrap();
}

fn spawn_wiring() -> (
    mpsc::UnboundedSender<AcpNotification>,
    mpsc::UnboundedReceiver<bool>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    AcpTuiClient,
) {
    let (client_transport, _server) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    let (notif_tx, notif_rx) = mpsc::unbounded_channel();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    let (observed_tx, observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let notifier =
        crate::kit::acp_notifier::spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
    let bridge = crate::kit::acp_bridge::spawn_acp_bridge_observed_with_client(
        bridge_rx,
        shutdown.clone(),
        client.clone(),
        observed_tx,
    );
    (notif_tx, observed_rx, shutdown, notifier, bridge, client)
}

async fn expect_observed(rx: &mut mpsc::UnboundedReceiver<bool>) {
    assert_eq!(
        peri_time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("bridge must process event"),
        Some(true)
    );
}

fn user_bubbles(items: &[TuiRenderUnit]) -> Vec<String> {
    items
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect()
}

/// B→A 的真实边界顺序 + assistant 输出跟在两者之后；重复 Delivered 幂等。
#[tokio::test]
#[serial_test::serial]
async fn delivered_bubbles_precede_assistant_at_real_bridge_boundary() {
    let _restore = RestoreAtoms::new();
    let (notif_tx, mut observed, shutdown, notifier, bridge, _client) = spawn_wiring();

    send_delivered(&notif_tx, "b", "B");
    expect_observed(&mut observed).await;
    send_delivered(&notif_tx, "a", "A");
    expect_observed(&mut observed).await;
    send_text(&notif_tx, "回答");
    expect_observed(&mut observed).await;
    // 重复交付确认不得再次产生气泡。
    send_delivered(&notif_tx, "a", "A");
    expect_observed(&mut observed).await;

    let items: Vec<TuiRenderUnit> = VIEW_MODELS.state().read().items.iter().cloned().collect();
    assert_eq!(user_bubbles(&items), vec!["B".to_string(), "A".to_string()]);
    let assistant_at = items
        .iter()
        .position(|unit| matches!(unit, TuiRenderUnit::TuiAssistantBubble(_)));
    assert!(
        assistant_at.is_some_and(|index| index >= 2),
        "assistant 输出必须排在 B、A 之后: {items:?}"
    );

    shutdown.cancel();
    drop(notif_tx);
    notifier.await.unwrap();
    bridge.await.unwrap();
}

/// assistant 已输出后，仅凭终态快照不得补一个位于回答之后的用户气泡。
#[tokio::test]
#[serial_test::serial]
async fn terminal_snapshot_after_assistant_output_never_appends_a_bubble() {
    let _restore = RestoreAtoms::new();
    let (notif_tx, mut observed, shutdown, notifier, bridge, _client) = spawn_wiring();

    send_text(&notif_tx, "先输出的回答");
    expect_observed(&mut observed).await;
    let before: Vec<TuiRenderUnit> = VIEW_MODELS.state().read().items.iter().cloned().collect();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s".into(),
            event: AcpEvent::UserInputQueueChanged {
                snapshot: UserInputQueueSnapshot {
                    session_id: "s".into(),
                    generation: "g".into(),
                    revision: 2,
                    active_request_id: None,
                    items: vec![],
                    terminal_items: vec![UserInputQueueTerminalItem {
                        input_id: "late".into(),
                        state: UserInputState::Delivered,
                    }],
                },
            },
        })
        .unwrap();
    expect_observed(&mut observed).await;

    let after: Vec<TuiRenderUnit> = VIEW_MODELS.state().read().items.iter().cloned().collect();
    assert_eq!(
        after.len(),
        before.len(),
        "终态快照只能更新提交反馈，不能合成气泡"
    );
    assert!(user_bubbles(&after).is_empty());

    shutdown.cancel();
    drop(notif_tx);
    notifier.await.unwrap();
    bridge.await.unwrap();
}

/// 致命执行失败（交付日志溢出等）必须结束提交反馈并留下可见错误。
#[tokio::test]
#[serial_test::serial]
async fn agent_execution_failure_clears_submitting_and_shows_error() {
    let _restore = RestoreAtoms::new();
    let epoch = BRIDGE_RESET_COUNTER.get();
    {
        let atom = STEERS.state();
        let mut steers = atom.write();
        steers.begin(SteerCommand {
            session_id: "s".into(),
            epoch,
            command_id: "failing-command".into(),
            generation: Some("g".into()),
            kind: SteerCommandKind::Enqueue(UserInput {
                input_id: "f".into(),
                content: MessageContent::text("F"),
                original_draft: "F".into(),
            }),
        });
        assert!(steers.direct_submitting("s", epoch));
    }
    let (notif_tx, mut observed, shutdown, notifier, bridge, _client) = spawn_wiring();
    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s".into(),
            event: AcpEvent::AgentExecutionFailed {
                message: "delivery journal overflow".into(),
            },
        })
        .unwrap();
    expect_observed(&mut observed).await;
    assert!(
        !STEERS.state().read().direct_submitting("s", epoch),
        "失败必须结束“正在提交…”"
    );
    assert!(
        STEERS.state().read().rows("s").is_empty(),
        "失败结算后 rows 不得继续投影 Submitting"
    );
    let items: Vec<TuiRenderUnit> = VIEW_MODELS.state().read().items.iter().cloned().collect();
    assert!(
        items
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
    );

    shutdown.cancel();
    drop(notif_tx);
    notifier.await.unwrap();
    bridge.await.unwrap();
}
