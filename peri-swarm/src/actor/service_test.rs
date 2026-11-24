//! Service-level approval tests: production SSE backlog, settlement durability
//! and ACP delivery failure.
//!
//! These tests own the real [`AcpSwarmService`] registry and a real [`ItemLog`];
//! only the ACP child is an in-memory transport, so the delivery boundary can
//! be open or closed without spawning a host.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse as _;
use serde_json::json;

use peri_acp::transport::mpsc::{mpsc_transport_pair, MpscServerTransport};
use peri_acp::transport::types::{AcpError, IncomingMessage};
use peri_acp::transport::AcpTransport as _;

use super::{AcpSwarmService, ActorServiceConfig, SessionRuntime, TurnSlot};
use crate::actor::reader::{ItemLog, TurnOutcome};
use crate::actor::runtime::AcpChild;
use crate::domain::{
    ApprovalDecision, ErrorCode, ErrorKind, EventCursor, InputKind, InputMode, InputSubmission,
    ItemKind, LogicalId, McpToolSpec, OfficialEvent, SessionState, StreamEvent,
};
use crate::service::SwarmService;
use crate::store::{MemorySwarmStore, SwarmItemRow, SwarmSessionRow, SwarmStore};

const LOGICAL_ID: &str = "agent-approval-test";

struct Fixture {
    service: Arc<AcpSwarmService>,
    log: Arc<ItemLog>,
    store: Arc<MemorySwarmStore>,
    runtime: Arc<SessionRuntime>,
    /// Keeps the in-memory transport's server side alive; dropping it closes the
    /// client write path and turns delivery into a false failure.
    server: MpscServerTransport,
}

fn tool(label: &str, require_approval: serde_json::Value) -> McpToolSpec {
    let mut spec = McpToolSpec::new(label, "https://mcp.example.test/mcp");
    spec.require_approval = Some(require_approval);
    spec
}

fn session_row(tools: Vec<McpToolSpec>) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: LOGICAL_ID.to_owned(),
        root_session_id: "0192f0c5-0000-7000-8000-000000000001".to_owned(),
        actor_key: format!("agent/{LOGICAL_ID}"),
        cwd: "/tmp".to_owned(),
        state: SessionState::Idle,
        model: None,
        instructions: None,
        mcp_tools: tools,
        environment: crate::domain::EnvironmentRef::none(),
        metadata: Default::default(),
        prompt_cache_options: None,
        created_at: "2026-11-21T00:00:00Z".to_owned(),
        last_activity_at: "2026-11-21T00:00:00Z".to_owned(),
        last_item_seq: 0,
    }
}

fn permission(server: &str, tool_name: &str) -> serde_json::Value {
    json!({
        "_meta": { "peri.mcp": { "server": server, "tool": tool_name } },
        "toolCall": {
            "toolCallId": "call-1",
            "title": tool_name,
            "rawInput": { "query": "x" },
        },
    })
}

fn logical_id() -> LogicalId {
    LogicalId::parse(LOGICAL_ID).expect("logical id")
}

async fn fixture(closed_transport: bool) -> Fixture {
    let tools = vec![tool("server-a", json!("always"))];
    let store = Arc::new(MemorySwarmStore::new());
    store
        .swarm_upsert_session(session_row(tools.clone()))
        .await
        .expect("session row");
    let service = Arc::new(AcpSwarmService::new(
        Arc::clone(&store) as Arc<dyn SwarmStore>,
        ActorServiceConfig::default(),
    ));
    let hub = service.hub_for(LOGICAL_ID);
    let log = Arc::new(
        ItemLog::new(
            LOGICAL_ID,
            Arc::clone(&store) as Arc<dyn SwarmStore>,
            hub.sender.clone(),
            Arc::new(AtomicU64::new(0)),
        )
        .with_approval_declarations(&tools),
    );
    let (client, server) = mpsc_transport_pair();
    if closed_transport {
        client.close();
    }
    let child = AcpChild::for_test(Arc::new(client), "sess-approval-test");
    let runtime = Arc::new(SessionRuntime::new(
        Arc::new(child),
        Arc::clone(&log),
        hub.sender.clone(),
    ));
    service
        .sessions
        .lock()
        .insert(LOGICAL_ID.to_owned(), Arc::clone(&runtime));
    Fixture {
        service,
        log,
        store,
        runtime,
        server,
    }
}

/// Open a turn and one declared `always` approval; returns the public id.
async fn pending_approval(fixture: &Fixture) -> String {
    fixture
        .log
        .begin_turn("prompt-1", "turn_1")
        .await
        .expect("turn opens");
    fixture
        .log
        .begin_permission(&json!(1), &permission("server-a", "search"))
        .await
        .expect("pending approval");
    fixture.log.required_actions()[0]["request_id"]
        .as_str()
        .expect("request id")
        .to_owned()
}

async fn approval_rows(fixture: &Fixture, request_id: &str) -> Vec<SwarmItemRow> {
    fixture
        .store
        .swarm_items_after(LOGICAL_ID, 0, 500)
        .await
        .expect("items")
        .into_iter()
        .filter(|row| {
            row.kind == ItemKind::Approval
                && row.payload.get("requestId").and_then(|id| id.as_str()) == Some(request_id)
        })
        .collect()
}

fn requires_action_snapshot(events: &[StreamEvent]) -> Option<(String, Vec<serde_json::Value>)> {
    events.iter().rev().find_map(|event| match event {
        StreamEvent::Official(OfficialEvent::RequiresAction { event_id, session }) => {
            Some((event_id.clone(), session.required_actions.clone()))
        }
        _ => None,
    })
}

#[tokio::test]
async fn a_reconnect_after_the_pending_row_still_receives_the_current_snapshot() {
    let fixture = fixture(false).await;
    let request_id = pending_approval(&fixture).await;
    let pending_seq = fixture
        .store
        .swarm_last_seq(LOGICAL_ID)
        .await
        .expect("last seq");
    assert!(
        approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.seq == pending_seq && row.payload["state"] == "pending"),
        "the opening approval row is the persisted tail"
    );

    // The cursor is *after* the opening row: replay alone carries nothing.
    let backlog = fixture
        .service
        .subscribe_backlog(&logical_id(), EventCursor::after(pending_seq))
        .await
        .expect("subscription backlog");
    assert!(
        backlog
            .iter()
            .all(|event| !matches!(event, StreamEvent::Item(_))),
        "replay after the tail has no item rows: {backlog:?}"
    );
    let (event_id, required_actions) =
        requires_action_snapshot(&backlog).expect("current requires_action snapshot");
    assert_eq!(
        event_id,
        crate::domain::official::requires_action_event_id(pending_seq),
        "the snapshot reuses the live event id derived from the pending row"
    );
    assert_eq!(required_actions.len(), 1);
    assert_eq!(required_actions[0]["request_id"], request_id);
}

#[tokio::test]
async fn an_expired_pending_approval_is_settled_before_the_replay_window() {
    let fixture = fixture(false).await;
    let request_id = pending_approval(&fixture).await;
    fixture.log.expire_approval_now(&request_id);

    // Replay from the start: the historical pending frame is followed by the
    // settlement, and the current snapshot must not resurrect the action.
    let backlog = fixture
        .service
        .subscribe_backlog(&logical_id(), EventCursor::START)
        .await
        .expect("subscription backlog");
    let (_, required_actions) =
        requires_action_snapshot(&backlog).expect("a requires_action frame");
    assert!(
        required_actions.is_empty(),
        "an expired action must not be the current snapshot: {required_actions:?}"
    );
    assert_eq!(
        fixture
            .service
            .get_session(&logical_id())
            .await
            .expect("session")
            .required_actions
            .len(),
        0
    );

    let rows = approval_rows(&fixture, &request_id).await;
    assert!(
        rows.iter()
            .any(|row| { row.payload["state"] == "settled" && row.payload["reason"] == "expired" }),
        "expiry must be persisted before replay: {rows:?}"
    );
}

#[tokio::test]
async fn a_settlement_persistence_failure_is_a_5xx_and_keeps_the_action_answerable() {
    let fixture = fixture(false).await;
    let request_id = pending_approval(&fixture).await;

    fixture.store.fail_next_settlement_appends(1);
    let error = fixture
        .service
        .respond_required_action(&logical_id(), &request_id, ApprovalDecision::Approve, None)
        .await
        .expect_err("an unpersisted settlement must not return a success receipt");
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert_eq!(error.code(), ErrorCode::RequiredActionSettlementFailed);
    assert_eq!(
        error.clone().into_response().status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        error.details()["requestId"],
        request_id,
        "the failure names the action: {error:?}"
    );
    assert_eq!(
        fixture.log.required_actions().len(),
        1,
        "the failed settlement must leave the pending fact answerable"
    );
    assert!(
        !approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled"),
        "no settlement row may exist after the injected store failure"
    );

    // The claim was released; storage recovered, so the retry settles.
    let receipt = fixture
        .service
        .respond_required_action(&logical_id(), &request_id, ApprovalDecision::Approve, None)
        .await
        .expect("retry settles once the store accepts the row");
    assert_eq!(receipt.request_id, request_id);
    assert!(fixture.log.required_actions().is_empty());
    assert!(
        approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled" && row.payload["decision"] == "approve"),
        "the successful retry persists the decision"
    );
}

#[tokio::test]
async fn an_expiry_settlement_persistence_failure_fails_the_read_and_is_retryable() {
    let fixture = fixture(false).await;
    let request_id = pending_approval(&fixture).await;
    fixture.log.expire_approval_now(&request_id);
    fixture.store.fail_next_settlement_appends(1);

    let error = fixture
        .service
        .get_session(&logical_id())
        .await
        .expect_err("a read cannot pretend an unpersisted expiry succeeded");
    assert_eq!(error.code(), ErrorCode::RequiredActionSettlementFailed);
    assert!(
        !approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled"),
        "the durable pending row was not replaced by a false settlement"
    );

    let view = fixture
        .service
        .get_session(&logical_id())
        .await
        .expect("retry persists the expiry");
    assert!(!matches!(
        view.status,
        crate::domain::SessionStatus::RequiresAction
    ));
    assert!(
        approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled" && row.payload["reason"] == "expired"),
        "the retry writes the durable expiry row"
    );
}

#[tokio::test]
async fn a_failed_acp_delivery_is_502_and_the_action_stays_answerable() {
    let fixture = fixture(true).await;
    let request_id = pending_approval(&fixture).await;

    let error = fixture
        .service
        .respond_required_action(&logical_id(), &request_id, ApprovalDecision::Approve, None)
        .await
        .expect_err("a closed transport cannot deliver the decision");
    assert_eq!(error.kind(), ErrorKind::BadGateway);
    assert_eq!(error.code(), ErrorCode::UpstreamUnavailable);
    assert_eq!(
        error.clone().into_response().status(),
        axum::http::StatusCode::BAD_GATEWAY
    );
    assert_eq!(error.details()["requestId"], request_id);
    assert_eq!(
        fixture.log.required_actions().len(),
        1,
        "502 must never describe a settled action"
    );
    assert!(
        !approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled"),
        "a failed delivery persists no settlement"
    );

    // The delivery claim was released: the retry observes the same delivery
    // failure, not a permanently reserved `delivering` action.
    let retry = fixture
        .service
        .respond_required_action(&logical_id(), &request_id, ApprovalDecision::Deny, None)
        .await
        .expect_err("the transport is still closed");
    assert_eq!(retry.code(), ErrorCode::UpstreamUnavailable);
    assert_eq!(retry.details()["requestId"], request_id);
}

#[tokio::test]
async fn a_delivered_approval_settles_and_leaves_no_pending_action() {
    let fixture = fixture(false).await;
    let request_id = pending_approval(&fixture).await;

    let receipt = fixture
        .service
        .respond_required_action(&logical_id(), &request_id, ApprovalDecision::Approve, None)
        .await
        .expect("the open transport accepts the decision");
    assert_eq!(receipt.request_id, request_id);
    assert_eq!(receipt.disposition, "inserted");
    assert!(fixture.log.required_actions().is_empty());
    assert!(
        approval_rows(&fixture, &request_id)
            .await
            .iter()
            .any(|row| row.payload["state"] == "settled"
                && row.payload["reason"] == "answered"
                && row.payload["decision"] == "approve"),
        "the durable row records the answer"
    );
}

/// One `mode=steer` user input for the service-level steer tests.
fn steer_submission(input_id: &str, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: input_id.to_owned(),
        kind: InputKind::User,
        mode: InputMode::Steer,
        text: Some(text.to_owned()),
        attachments: Vec::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

/// Open the turn the steer path appends to: an active log turn and the
/// runtime's turn slot.
async fn open_turn(runtime: &SessionRuntime) {
    runtime
        .log
        .begin_turn("req-1", "turn_1")
        .await
        .expect("turn opens");
    runtime.turn.send_replace(Some(TurnSlot {
        request_id: "req-1".to_owned(),
        started_at: "2026-11-24T00:00:00Z".to_owned(),
    }));
}

/// Round 9 finding 2: two concurrent steers must reach the host in the order
/// their generations were reserved. The reader claims the oldest unclaimed
/// generation for the next run that starts, so a handoff that overtakes the
/// earlier reservation would make the claim name the wrong input.
///
/// The first snapshot reply is held until the test releases it, and the release
/// happens after a bounded wait for the responder to see a second snapshot.
/// With the service lock only one handoff can exist, so that wait always times
/// out; it is not a scheduling assumption, it only adds its bound to the locked
/// path. Without the lock the second handoff reaches the host at once, the
/// first reply is released immediately, and the second input dispatches first —
/// the failing path.
#[tokio::test]
async fn concurrent_steers_dispatch_in_reservation_order() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime).await;

    let server = Arc::new(server);
    let (snapshot_tx, mut snapshot_rx) = tokio::sync::mpsc::unbounded_channel::<u8>();
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
    let (dispatched_tx, mut dispatched_rx) = tokio::sync::mpsc::unbounded_channel();
    let responder = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let mut first_snapshot = true;
            let mut release_first_rx = Some(release_first_rx);
            loop {
                let Some(message) = server.recv().await else {
                    break;
                };
                let IncomingMessage::Request { id, method, params } = message else {
                    panic!("unexpected host message");
                };
                match method.as_str() {
                    "session/input/snapshot" if first_snapshot => {
                        first_snapshot = false;
                        snapshot_tx
                            .send(1)
                            .expect("the test waits for the first snapshot");
                        // Hold the first reply only; the responder keeps
                        // serving, so without the service lock the second
                        // handoff reaches this loop right away.
                        let release = release_first_rx.take().expect("release signal");
                        let server = Arc::clone(&server);
                        tokio::spawn(async move {
                            release.await.expect("the test releases the first reply");
                            server
                                .send_response(id, Ok(json!({ "generation": "gen-1" })))
                                .await
                                .expect("delayed snapshot response");
                        });
                    }
                    "session/input/snapshot" => {
                        let _ = snapshot_tx.send(2);
                        server
                            .send_response(id, Ok(json!({ "generation": "gen-1" })))
                            .await
                            .expect("snapshot response");
                    }
                    "session/input/enqueue" => {
                        server
                            .send_response(id, Ok(json!({ "ok": true })))
                            .await
                            .expect("enqueue response");
                    }
                    "session/input/dispatch" => {
                        let input_id = params["inputIds"][0]
                            .as_str()
                            .expect("dispatched input id")
                            .to_owned();
                        dispatched_tx.send(input_id).expect("dispatch order");
                        server
                            .send_response(id, Ok(json!({ "ok": true })))
                            .await
                            .expect("dispatch response");
                    }
                    other => panic!("unexpected host request: {other}"),
                }
            }
        })
    };

    let u1 = {
        let service = Arc::clone(&service);
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            service
                .steer(
                    &runtime,
                    &steer_submission("u1", "first"),
                    &logical_id(),
                    "turn_1",
                    "req-1",
                )
                .await
        })
    };
    let first = tokio::time::timeout(Duration::from_secs(5), snapshot_rx.recv())
        .await
        .expect("the first steer reached the snapshot RPC");
    assert_eq!(first, Some(1), "the first snapshot signal");
    let u2 = {
        let service = Arc::clone(&service);
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            service
                .steer(
                    &runtime,
                    &steer_submission("u2", "second"),
                    &logical_id(),
                    "turn_1",
                    "req-1",
                )
                .await
        })
    };
    // Bounded wait for the second handoff. With the lock u2 is still waiting
    // for it, so this expires; without it the handoff is already at the host.
    let _second_handoff = tokio::time::timeout(Duration::from_secs(1), snapshot_rx.recv()).await;
    release_first_tx
        .send(())
        .expect("the test releases the first reply");
    let first = tokio::time::timeout(Duration::from_secs(5), u1)
        .await
        .expect("u1 does not hang")
        .expect("u1 task")
        .expect("u1 steer");
    let second = tokio::time::timeout(Duration::from_secs(5), u2)
        .await
        .expect("u2 does not hang")
        .expect("u2 task")
        .expect("u2 steer");
    assert_eq!(first.input_id, "u1");
    assert_eq!(second.input_id, "u2");

    let order: Vec<String> = std::iter::from_fn(|| dispatched_rx.try_recv().ok()).collect();
    assert_eq!(
        order,
        ["u1", "u2"],
        "dispatch order must follow reservation order"
    );

    // Closing the client ends the responder's receive loop.
    drop(service);
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("the responder stops once the client closes")
        .expect("responder");
}

/// Round 9 finding 2: a failed dispatch rolls the reserved generation back
/// under the same lock, so no generation stays unresolved and the next steer
/// still advances.
#[tokio::test]
async fn a_failed_dispatch_rolls_back_the_steer_and_the_next_one_advances() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime).await;

    let server = Arc::new(server);
    let responder = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                let Some(message) = server.recv().await else {
                    break;
                };
                let IncomingMessage::Request { id, method, params } = message else {
                    panic!("unexpected host message");
                };
                let result = match method.as_str() {
                    "session/input/snapshot" => Ok(json!({ "generation": "gen-1" })),
                    "session/input/enqueue" => Ok(json!({ "ok": true })),
                    "session/input/dispatch" if params["inputIds"][0] == "u1" => {
                        Err(AcpError::new(-32602, "dispatch rejected"))
                    }
                    "session/input/dispatch" => Ok(json!({ "ok": true })),
                    other => panic!("unexpected host request: {other}"),
                };
                server.send_response(id, result).await.expect("response");
            }
        })
    };

    let error = service
        .steer(
            &runtime,
            &steer_submission("u1", "first"),
            &logical_id(),
            "turn_1",
            "req-1",
        )
        .await
        .expect_err("the rejected dispatch fails the steer");
    assert_eq!(error.code(), ErrorCode::QueueUnavailable);
    assert_eq!(error.details()["operation"], "session/input/dispatch");
    assert!(
        !runtime.log.steer_pending(),
        "the rollback must leave no steer owning the turn"
    );
    assert_eq!(runtime.log.steer_generation(), 1);

    let next = service
        .steer(
            &runtime,
            &steer_submission("u2", "second"),
            &logical_id(),
            "turn_1",
            "req-1",
        )
        .await
        .expect("the next steer still advances");
    assert_eq!(next.input_id, "u2");
    assert_eq!(runtime.log.steer_generation(), 2);
    assert!(
        runtime.log.steer_pending(),
        "the dispatched steer owns the turn until its run starts"
    );

    // Closing the client ends the responder's receive loop.
    drop(service);
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("the responder stops once the client closes")
        .expect("responder");
}

/// Round 10 finding 1: a steer admitted into a turn that has since closed must
/// not be dispatched into the turn that replaced it. The reservation names the
/// admission turn, so a different open turn yields `turn_in_progress` and no
/// handoff at all.
#[tokio::test]
async fn a_steer_admitted_for_a_closed_turn_is_rejected_instead_of_dispatched() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    // The session moved on: `req-1` is gone and `req-2` owns the turn slot.
    runtime
        .log
        .begin_turn("req-2", "turn_2")
        .await
        .expect("turn opens");
    runtime.turn.send_replace(Some(TurnSlot {
        request_id: "req-2".to_owned(),
        started_at: "2026-11-24T00:00:00Z".to_owned(),
    }));

    let server = Arc::new(server);
    let (dispatched_tx, mut dispatched_rx) = tokio::sync::mpsc::unbounded_channel();
    let responder = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                let Some(message) = server.recv().await else {
                    break;
                };
                let IncomingMessage::Request { id, method, params } = message else {
                    panic!("unexpected host message");
                };
                let result = match method.as_str() {
                    "session/input/snapshot" => Ok(json!({ "generation": "gen-1" })),
                    "session/input/enqueue" => Ok(json!({ "ok": true })),
                    "session/input/dispatch" => {
                        let input_id = params["inputIds"][0]
                            .as_str()
                            .expect("dispatched input id")
                            .to_owned();
                        dispatched_tx.send(input_id).expect("dispatch order");
                        Ok(json!({ "ok": true }))
                    }
                    other => panic!("unexpected host request: {other}"),
                };
                server.send_response(id, result).await.expect("response");
            }
        })
    };

    let result = service
        .steer(
            &runtime,
            &steer_submission("u1", "stale"),
            &logical_id(),
            "turn_1",
            "req-1",
        )
        .await;
    // The reservation is checked before the snapshot/enqueue/dispatch handoff,
    // so the rejected steer reaches no host RPC; only the dispatch is asserted
    // because that is the frame that would attribute the input to `req-2`.
    let dispatched = dispatched_rx.try_recv().ok();
    assert!(
        dispatched.is_none(),
        "the stale input must not be dispatched into the other turn: {dispatched:?}"
    );
    let error = result.expect_err("a steer admitted for the closed turn is rejected");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert_eq!(
        runtime.log.steer_generation(),
        0,
        "a rejected steer reserves no generation"
    );
    assert!(
        !runtime.log.steer_pending(),
        "a rejected steer must not own the other turn"
    );
    assert_eq!(
        runtime.log.active_turn().as_deref(),
        Some("req-2"),
        "the replacement turn keeps the session"
    );

    // Closing the client ends the responder's receive loop.
    drop(service);
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("the responder stops once the client closes")
        .expect("responder");
}

/// Round 11: the turn `accept_input` admits an input into is the turn the steer
/// reservation names. The other steer tests call `steer` directly, so a
/// constant `admitted_turn` would reach the same `Reserved` branch and leave
/// them green; only a handoff through `accept_input` pins the argument. The
/// rejection *is* constructible from this layer — hold `runtime.steer` and
/// replace the turn while admission waits (see the negative test below) — but a
/// constant rejects with the same `turn_in_progress`, so the mutant witness is
/// this positive dispatch, not the rejection.
#[tokio::test]
async fn an_accepted_steer_is_dispatched_with_the_admission_turn() {
    use crate::domain::Disposition;

    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime).await;

    let server = Arc::new(server);
    let (dispatched_tx, mut dispatched_rx) = tokio::sync::mpsc::unbounded_channel();
    let responder = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                let Some(message) = server.recv().await else {
                    break;
                };
                let IncomingMessage::Request { id, method, params } = message else {
                    panic!("unexpected host message");
                };
                let result = match method.as_str() {
                    "session/input/snapshot" => Ok(json!({ "generation": "gen-1" })),
                    "session/input/enqueue" => Ok(json!({ "ok": true })),
                    "session/input/dispatch" => {
                        let input_id = params["inputIds"][0]
                            .as_str()
                            .expect("dispatched input id")
                            .to_owned();
                        dispatched_tx.send(input_id).expect("dispatch order");
                        Ok(json!({ "ok": true }))
                    }
                    other => panic!("unexpected host request: {other}"),
                };
                server.send_response(id, result).await.expect("response");
            }
        })
    };

    let accepted = service
        .accept_input(&logical_id(), steer_submission("u1", "first"))
        .await
        .expect("an input admitted into the open turn is accepted");
    assert_eq!(accepted.outcome.input_id, "u1");
    assert_eq!(
        accepted.outcome.disposition,
        Disposition::Inserted,
        "a running turn takes the steer by dispatch, not by opening a turn"
    );
    assert!(
        accepted.pending_turn.is_none(),
        "the steer ran in `accept_input`; there is no pending turn to start"
    );
    let dispatched = tokio::time::timeout(Duration::from_secs(5), dispatched_rx.recv())
        .await
        .expect("the handoff does not wait for a turn terminal")
        .expect("the responder reports the dispatch");
    assert_eq!(
        dispatched, "u1",
        "accept_input must dispatch the input it admitted"
    );
    assert!(
        dispatched_rx.try_recv().is_err(),
        "one accepted input reaches the host as exactly one dispatch"
    );
    assert_eq!(
        runtime.log.steer_generation(),
        1,
        "the dispatch reserves the generation"
    );
    assert!(
        runtime.log.steer_pending(),
        "the dispatched steer owns the turn until its run starts"
    );
    assert_eq!(
        runtime.log.active_turn().as_deref(),
        Some("req-1"),
        "the admitted turn keeps the session"
    );

    // Closing the client ends the responder's receive loop.
    drop(service);
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("the responder stops once the client closes")
        .expect("responder");
}

/// Round 11 rework: a steer admitted into the turn observed at admission is
/// rejected when that turn is replaced while the input waits for the steer lock.
/// The interleaving is deterministic: this test holds `runtime.steer` before
/// `accept_input` reaches it, waits for the receipt (written after the admission
/// snapshot), then closes `req-1`, opens `req-2` and publishes the slot under
/// `admission`, which can only be taken after `accept_input` released it. The
/// positive test above is the mutant witness; this one cannot be — a constant
/// `admitted_turn` rejects with the same `turn_in_progress`, so it does not
/// distinguish the mutation.
#[tokio::test]
async fn a_steer_accepted_into_a_turn_that_is_replaced_while_waiting_is_rejected() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime).await;

    // Park `accept_input` after its admission snapshot but before the steer
    // lock: the handoff needs the same lock.
    let steer_guard = runtime.steer.lock().await;

    let server = Arc::new(server);
    let (dispatched_tx, mut dispatched_rx) = tokio::sync::mpsc::unbounded_channel();
    let responder = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                let Some(message) = server.recv().await else {
                    break;
                };
                let IncomingMessage::Request { id, method, params } = message else {
                    panic!("unexpected host message");
                };
                let result = match method.as_str() {
                    "session/input/snapshot" => Ok(json!({ "generation": "gen-1" })),
                    "session/input/enqueue" => Ok(json!({ "ok": true })),
                    "session/input/dispatch" => {
                        let input_id = params["inputIds"][0]
                            .as_str()
                            .expect("dispatched input id")
                            .to_owned();
                        dispatched_tx.send(input_id).expect("dispatch order");
                        Ok(json!({ "ok": true }))
                    }
                    other => panic!("unexpected host request: {other}"),
                };
                server.send_response(id, result).await.expect("response");
            }
        })
    };

    let accepting = {
        let service = Arc::clone(&service);
        let logical_id = logical_id();
        let submission = steer_submission("u1", "stale");
        tokio::spawn(async move { service.accept_input(&logical_id, submission).await })
    };

    // The receipt is written after the admission snapshot and before the steer
    // lock, so seeing it proves the snapshot names `req-1` and the input is
    // parked on the lock this test holds.
    let receipt_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if store
            .swarm_get_input(LOGICAL_ID, "u1")
            .await
            .expect("input receipt")
            .is_some()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < receipt_deadline,
            "accept_input did not write the receipt within the bound"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // `begin_turn` rejects while another turn is active, so close `req-1` first
    // (the real close path takes `state.turn_id`), then open the replacement.
    // The slot is published under `admission`, which can only be taken after
    // `accept_input` released it; that ordering is what makes the race
    // deterministic.
    runtime
        .log
        .finish_turn("req-1", TurnOutcome::Completed("end_turn".to_owned()))
        .await;
    runtime
        .log
        .begin_turn("req-2", "turn_2")
        .await
        .expect("turn opens");
    {
        let _admission = runtime.admission.lock().await;
        runtime.turn.send_replace(Some(TurnSlot {
            request_id: "req-2".to_owned(),
            started_at: "2026-11-24T00:00:00Z".to_owned(),
        }));
    }
    drop(steer_guard);

    let result = tokio::time::timeout(Duration::from_secs(5), accepting)
        .await
        .expect("the parked handoff is released")
        .expect("accept task");
    let error = result.expect_err("an input admitted into the replaced turn is rejected");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        dispatched_rx.try_recv().is_err(),
        "a rejected steer must not reach the host"
    );
    assert_eq!(
        runtime.log.steer_generation(),
        0,
        "a rejected steer reserves no generation"
    );
    assert!(
        !runtime.log.steer_pending(),
        "a rejected steer must not own the replacement turn"
    );
    assert_eq!(
        runtime.log.active_turn().as_deref(),
        Some("req-2"),
        "the replacement turn keeps the session"
    );

    // Closing the client ends the responder's receive loop.
    drop(service);
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(5), responder)
        .await
        .expect("the responder stops once the client closes")
        .expect("responder");
}
