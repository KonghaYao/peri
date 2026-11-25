//! Service-level approval tests: production SSE backlog, settlement durability
//! and ACP delivery failure.
//!
//! These tests own the real [`AcpSwarmService`] registry and a real [`ItemLog`];
//! only the ACP child is an in-memory transport, so the delivery boundary can
//! be open or closed without spawning a host.

use std::future::Future;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse as _;
use futures::FutureExt as _;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use peri_acp::transport::mpsc::{mpsc_transport_pair, MpscServerTransport};
use peri_acp::transport::types::{AcpError, IncomingMessage, RequestId};
use peri_acp::transport::AcpTransport as _;

use super::{AcpSwarmService, ActorServiceConfig, SessionRuntime, TurnSlot};
use crate::actor::reader::{ItemLog, TurnOutcome};
use crate::actor::runtime::AcpChild;
use crate::domain::{
    ApprovalDecision, Disposition, ErrorCode, ErrorKind, EventCursor, InputKind, InputMode,
    InputState, InputSubmission, ItemKind, LogicalId, McpToolSpec, OfficialEvent, SessionState,
    StreamEvent, SwarmError,
};
use crate::service::{AcceptedInput, SwarmService};
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

async fn open_turn(runtime: &SessionRuntime, request_id: &str, turn_id: &str) {
    runtime
        .log
        .begin_turn(request_id, turn_id)
        .await
        .expect("turn opens");
    runtime.turn.send_replace(Some(TurnSlot {
        request_id: request_id.to_owned(),
        started_at: "2026-11-24T00:00:00Z".to_owned(),
    }));
}

async fn close_first_turn(runtime: &SessionRuntime) {
    runtime
        .log
        .finish_turn("req-1", TurnOutcome::Completed("end_turn".to_owned()))
        .await;
    assert!(runtime.log.active_turn().is_none());
    runtime.turn.send_replace(None);
}

async fn bounded<T>(future: impl Future<Output = T>, reason: &str) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect(reason)
}

fn spawn_steer(
    service: &Arc<AcpSwarmService>,
    submission: InputSubmission,
) -> JoinHandle<Result<AcceptedInput, SwarmError>> {
    let service = Arc::clone(service);
    tokio::spawn(async move { service.accept_input(&logical_id(), submission).await })
}

async fn wait_for_steer_lock(runtime: &SessionRuntime) {
    // 测试使用单线程 runtime：通知后没有其他 await，持锁时调用方必然先 poll 取锁再让出。
    bounded(
        runtime.steer_waiting.notified(),
        "accept_input reached the steer lock",
    )
    .await;
}

async fn host_request(server: &MpscServerTransport, expected: &str) -> (RequestId, Value) {
    match bounded(server.recv(), expected)
        .await
        .expect("host request")
    {
        IncomingMessage::Request { id, method, params } => {
            assert_eq!(method, expected);
            (id, params)
        }
        other => panic!("unexpected host message: {other:?}"),
    }
}

async fn host_ok(server: &MpscServerTransport, id: RequestId, result: Value) {
    server
        .send_response(id, Ok(result))
        .await
        .expect("host response");
}

async fn reach_dispatch(server: &MpscServerTransport) -> (RequestId, String, String) {
    let (snapshot, _) = host_request(server, "session/input/snapshot").await;
    host_ok(server, snapshot, json!({ "generation": "gen-1" })).await;
    reach_dispatch_after_snapshot(server).await
}

async fn reach_dispatch_after_snapshot(
    server: &MpscServerTransport,
) -> (RequestId, String, String) {
    let (enqueue, params) = host_request(server, "session/input/enqueue").await;
    let enqueued = params["inputId"]
        .as_str()
        .expect("enqueue input id")
        .to_owned();
    host_ok(server, enqueue, json!({ "ok": true })).await;
    let (dispatch, params) = host_request(server, "session/input/dispatch").await;
    let dispatched = params["inputIds"][0]
        .as_str()
        .expect("dispatch input id")
        .to_owned();
    (dispatch, enqueued, dispatched)
}

async fn acknowledge_steer(server: &MpscServerTransport, input_id: &str) {
    let (dispatch, enqueued, dispatched) = reach_dispatch(server).await;
    assert_eq!(enqueued, input_id);
    assert_eq!(dispatched, input_id);
    host_ok(server, dispatch, json!({ "ok": true })).await;
}

fn assert_no_host_request(server: &MpscServerTransport) {
    assert!(
        server.recv().now_or_never().is_none(),
        "the input must not reach any host RPC"
    );
}

async fn assert_unwritten(store: &MemorySwarmStore, input_id: &str) {
    assert!(
        store
            .swarm_get_input(LOGICAL_ID, input_id)
            .await
            .expect("read receipt")
            .is_none(),
        "the input must not leave an accepted receipt"
    );
    assert!(
        store
            .swarm_get_item(LOGICAL_ID, &format!("itim_input_{input_id}"))
            .await
            .expect("read user item")
            .is_none(),
        "the input must not leave a user item"
    );
}

async fn assert_written(store: &MemorySwarmStore, submission: &InputSubmission) -> u64 {
    let receipt = store
        .swarm_get_input(LOGICAL_ID, &submission.input_id)
        .await
        .expect("read receipt")
        .expect("accepted receipt");
    assert_eq!(receipt.state, InputState::Accepted);
    assert_eq!(receipt.fingerprint, submission.fingerprint());
    let item_id = format!("itim_input_{}", submission.input_id);
    let items = store
        .swarm_items_after(LOGICAL_ID, 0, 100)
        .await
        .expect("read items");
    let mut items = items.iter().filter(|item| item.item_id == item_id);
    let item = items.next().expect("accepted user item");
    assert_eq!(item.payload["role"], "user");
    assert_eq!(item.payload["text"], submission.effective_text());
    assert!(
        items.next().is_none(),
        "only one user item is written for this input"
    );
    item.seq
}

async fn finish_steered_turn(runtime: &SessionRuntime) {
    let request_id = "queued-run";
    runtime.log.ingest("peri/agent_event", &json!({
        "event_json": json!({"type": "user_input_run_started", "value": {"request_id": request_id}}).to_string()
    })).await;
    runtime
        .log
        .ingest(
            "peri/agent_event_done",
            &json!({
                "sessionId": LOGICAL_ID, "stopReason": "end_turn", "requestId": request_id
            }),
        )
        .await;
    assert!(
        runtime.log.active_turn().is_none(),
        "the dispatched run must claim the generation and close the turn"
    );
    assert!(!runtime.log.steer_pending());
}

#[test]
fn concurrent_steers_dispatch_in_call_order() {
    let caller = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("caller runtime");
    let mut gated = Some(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("gated runtime"),
    );
    caller.block_on(async {
        let Fixture {
            service,
            runtime,
            server,
            ..
        } = fixture(false).await;
        open_turn(&runtime, "req-1", "turn_1").await;
        let logical_id = logical_id();
        let mut u1 =
            std::pin::pin!(service.accept_input(&logical_id, steer_submission("u1", "first")));
        {
            // 调用方先 poll u1，但让它 spawn 的任务进一个暂不驱动的 executor。
            let _entered = gated.as_ref().expect("gated runtime").enter();
            assert!(u1.as_mut().now_or_never().is_none());
        }
        wait_for_steer_lock(&runtime).await;
        let caller_locked_first = runtime.steer.try_lock().is_err();
        let u2 = spawn_steer(&service, steer_submission("u2", "second"));
        wait_for_steer_lock(&runtime).await;

        let mut enqueue_order = Vec::new();
        let mut dispatch_order = Vec::new();
        if !caller_locked_first {
            // 若 u1 不在调用方取锁，必须先实际收到 u2 的派发，才允许 u1 的任务运行。
            let (dispatch, enqueued, dispatched) = reach_dispatch(&server).await;
            assert_eq!(dispatched, "u2", "u1 的 executor 尚未运行");
            enqueue_order.push(enqueued);
            dispatch_order.push(dispatched);
            host_ok(&server, dispatch, json!({ "ok": true })).await;
        }
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let gated = gated.take().expect("gated runtime");
        let worker = tokio::task::spawn_blocking(move || gated.block_on(stopped));

        let (dispatch, enqueued, dispatched) = reach_dispatch(&server).await;
        assert!(
            runtime.steer.try_lock().is_err(),
            "u1 handoff still holds the steer lock at dispatch"
        );
        enqueue_order.push(enqueued);
        dispatch_order.push(dispatched);
        host_ok(&server, dispatch, json!({ "ok": true })).await;
        if caller_locked_first {
            let (dispatch, enqueued, dispatched) = reach_dispatch(&server).await;
            enqueue_order.push(enqueued);
            dispatch_order.push(dispatched);
            host_ok(&server, dispatch, json!({ "ok": true })).await;
        }
        let first = bounded(u1, "u1 completes").await.expect("u1 accepted");
        let second = bounded(u2, "u2 completes")
            .await
            .expect("u2 joined")
            .expect("u2 accepted");
        stop.send(()).expect("gated runtime still drives u1");
        bounded(worker, "gated runtime stops")
            .await
            .expect("worker joined")
            .expect("stop received");

        assert_eq!(first.outcome.input_id, "u1");
        assert_eq!(second.outcome.input_id, "u2");
        assert_eq!(
            dispatch_order,
            ["u1", "u2"],
            "dispatch order must follow call order"
        );
        assert_eq!(
            enqueue_order,
            ["u1", "u2"],
            "enqueue order must follow call order"
        );
        assert_no_host_request(&server);
    });
}

#[tokio::test]
async fn a_failed_dispatch_rolls_back_the_steer_and_the_next_one_advances() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;
    let u1 = spawn_steer(&service, steer_submission("u1", "first"));
    let (dispatch, _, dispatched) = reach_dispatch(&server).await;
    assert_eq!(dispatched, "u1");
    reject_dispatch(&server, dispatch).await;
    let error = bounded(u1, "dispatch failure returns")
        .await
        .expect("u1 joined")
        .expect_err("dispatch rejected");
    assert_eq!(error.code(), ErrorCode::QueueUnavailable);
    assert_eq!(error.details()["operation"], "session/input/dispatch");
    assert!(!runtime.log.steer_pending());
    assert_eq!(runtime.log.steer_generation(), 1);

    let u2 = spawn_steer(&service, steer_submission("u2", "second"));
    acknowledge_steer(&server, "u2").await;
    assert_eq!(
        bounded(u2, "u2 completes")
            .await
            .expect("u2 joined")
            .expect("u2 accepted")
            .outcome
            .disposition,
        Disposition::Inserted
    );
    assert_eq!(runtime.log.steer_generation(), 2);
    finish_steered_turn(&runtime).await;
}

async fn reject_dispatch(server: &MpscServerTransport, dispatch: RequestId) {
    server
        .send_response(dispatch, Err(AcpError::new(-32602, "dispatch rejected")))
        .await
        .expect("dispatch error sent");
}

#[tokio::test]
async fn aborting_after_handoff_still_rolls_back_a_dispatch_error() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;
    let accepting = spawn_steer(&service, steer_submission("u1", "first"));
    let (dispatch, _, dispatched) = reach_dispatch(&server).await;
    assert_eq!(dispatched, "u1");
    assert!(runtime.log.steer_pending());
    abort_accept(accepting).await;
    reject_dispatch(&server, dispatch).await;
    // 下一个持锁者只能在独立任务完成派发失败处理之后到达。
    let _done = bounded(runtime.steer.lock(), "detached handoff releases steer").await;
    assert!(
        !runtime.log.steer_pending(),
        "dispatch failure must release the unresolved generation even without a caller"
    );
    assert_eq!(runtime.log.steer_generation(), 1);
    assert_no_host_request(&server);
}

async fn abort_accept(accepting: JoinHandle<Result<AcceptedInput, SwarmError>>) {
    accepting.abort();
    let error = bounded(accepting, "accept_input cancellation completes")
        .await
        .expect_err("caller was cancelled");
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn aborting_before_steer_handoff_leaves_no_trace_and_can_retry() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;
    let guard = runtime.steer.lock().await;
    let submission = steer_submission("u1", "first");
    let accepting = spawn_steer(&service, submission.clone());
    wait_for_steer_lock(&runtime).await;
    assert!(runtime.admission.try_lock().is_ok());
    abort_accept(accepting).await;
    drop(guard);
    assert_unwritten(&store, "u1").await;
    assert_eq!(runtime.log.steer_generation(), 0);
    assert_no_host_request(&server);

    let retry = spawn_steer(&service, submission.clone());
    acknowledge_steer(&server, "u1").await;
    assert_eq!(
        bounded(retry, "retry completes")
            .await
            .expect("retry joined")
            .expect("retry accepted")
            .outcome
            .disposition,
        Disposition::Inserted
    );
    assert_written(&store, &submission).await;
    assert_eq!(runtime.log.steer_generation(), 1);
    assert_no_host_request(&server);
}

#[tokio::test]
async fn aborting_after_steer_handoff_still_dispatches_and_continues_the_turn() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;
    let submission = steer_submission("u1", "first");
    let accepting = spawn_steer(&service, submission.clone());
    let (snapshot, _) = host_request(&server, "session/input/snapshot").await;
    assert_written(&store, &submission).await;
    assert!(runtime.log.steer_pending());
    assert!(runtime.steer.try_lock().is_err());
    abort_accept(accepting).await;
    assert!(
        runtime.steer.try_lock().is_err(),
        "the caller no longer owns the guard"
    );

    host_ok(&server, snapshot, json!({ "generation": "gen-1" })).await;
    let (dispatch, enqueued, dispatched) = reach_dispatch_after_snapshot(&server).await;
    assert_eq!(enqueued, "u1");
    assert_eq!(dispatched, "u1");
    host_ok(&server, dispatch, json!({ "ok": true })).await;
    let _done = bounded(runtime.steer.lock(), "detached handoff finishes").await;
    assert_written(&store, &submission).await;
    assert_eq!(runtime.log.steer_generation(), 1);
    finish_steered_turn(&runtime).await;
    assert_no_host_request(&server);
}

#[tokio::test]
async fn an_accepted_steer_is_dispatched_with_the_admission_turn() {
    let Fixture {
        service,
        runtime,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-live", "turn_live").await;
    let responder = tokio::spawn(async move {
        acknowledge_steer(&server, "u1").await;
        server
    });

    let accepted = bounded(
        service.accept_input(&logical_id(), steer_submission("u1", "first")),
        "steer acceptance does not wait for a turn terminal",
    )
    .await
    .expect("an input admitted into the open turn is accepted");
    assert_eq!(accepted.outcome.input_id, "u1");
    assert_eq!(accepted.outcome.disposition, Disposition::Inserted);
    assert!(accepted.pending_turn.is_none());
    let server = bounded(responder, "the host reports the dispatch")
        .await
        .expect("responder joined");
    assert_no_host_request(&server);
    assert_eq!(runtime.log.steer_generation(), 1);
    assert!(runtime.log.steer_pending());
    assert_eq!(runtime.log.active_turn().as_deref(), Some("req-live"));
}

/// 第 12 轮（C3）：准入快照里的 turn 在等待 steer 锁期间被替换时，handoff 在锁内拒绝且
/// 零写入。交错是确定性的：测试先持有 `runtime.steer`，`steer_waiting` 通知证明调用方已读完
/// 准入快照（`req-1`）并停在锁上；随后关闭 `req-1`、打开替换者并在 `admission` 内发布
/// `req-2`，再放锁。拒绝后同一 `inputId`/fingerprint 在当前 running turn 上重试必须被真正
/// 派发——receipt 未写，所以 `existing_input` 不会把这次重试当成已受理。
///
/// 正例（`an_accepted_steer_is_dispatched_with_the_admission_turn`）抓“准入 turn 常量”
/// 变异；本条不抓：常量实参在这里同样返回 `turn_in_progress`。
#[tokio::test]
async fn a_steer_accepted_into_a_turn_that_is_replaced_while_waiting_is_rejected() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;

    let steer_guard = runtime.steer.lock().await;
    let submission = steer_submission("u1", "stale");
    let accepting = spawn_steer(&service, submission.clone());
    wait_for_steer_lock(&runtime).await;
    assert!(
        runtime.admission.try_lock().is_ok(),
        "the caller queues on the steer lock only after releasing admission"
    );

    // `begin_turn` 拒绝已打开的 turn，因此先关闭 `req-1`（真实关闭路径取走 `turn_id`），
    // 再打开替换者并在 `admission` 内发布 slot。`accept_input` 已释放 admission，顺序无竞态。
    close_first_turn(&runtime).await;
    runtime
        .log
        .begin_turn("req-2", "turn_2")
        .await
        .expect("replacement turn opens");
    {
        let _admission = runtime.admission.lock().await;
        runtime.turn.send_replace(Some(TurnSlot {
            request_id: "req-2".to_owned(),
            started_at: "2026-11-24T00:00:00Z".to_owned(),
        }));
    }
    drop(steer_guard);

    let error = bounded(accepting, "the parked handoff is released")
        .await
        .expect("accept task")
        .expect_err("an input admitted into the replaced turn is rejected");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert_eq!(
        error.details().get("suggest"),
        None,
        "the admitted turn ended: the same inputId may be retried, steering is not the fix"
    );
    assert_no_host_request(&server);
    assert_unwritten(&store, "u1").await;
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

    // 零写入让同一 inputId 的重试在当前 turn 上重新准入并被派发。
    let retry = spawn_steer(&service, submission.clone());
    acknowledge_steer(&server, "u1").await;
    let accepted = bounded(retry, "the same inputId is retried into the current turn")
        .await
        .expect("retry joined")
        .expect("a clean rejection leaves the inputId retryable");
    assert_eq!(accepted.outcome.input_id, "u1");
    assert_eq!(accepted.outcome.disposition, Disposition::Inserted);
    assert!(accepted.pending_turn.is_none());
    assert_written(&store, &submission).await;
    assert_eq!(runtime.log.steer_generation(), 1);
    assert!(runtime.log.steer_pending());
    assert_eq!(runtime.log.active_turn().as_deref(), Some("req-2"));
    assert_no_host_request(&server);
}

/// 同 id 并发对：同一 running turn、同 id/fingerprint 的两个受理恰好一个 `Inserted`，
/// 另一个在锁内看到 receipt 返回 `Existing`。断言只有一条 receipt、一条 user item、
/// 一次 enqueue/dispatch，且重复受理不额外预留 generation（task 内的同 id 预检查）。
#[tokio::test]
async fn concurrent_duplicates_of_one_input_dispatch_once() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;
    let submission = steer_submission("u1", "same");
    let first = spawn_steer(&service, submission.clone());
    let second = spawn_steer(&service, submission.clone());

    // 恰好一组 snapshot/enqueue/dispatch：重复者不允许再发 host RPC。
    acknowledge_steer(&server, "u1").await;
    let first = bounded(first, "the first duplicate returns")
        .await
        .expect("first joined")
        .expect("first accepted");
    let second = bounded(second, "the second duplicate returns")
        .await
        .expect("second joined")
        .expect("second accepted");

    assert_eq!(first.outcome.input_id, "u1");
    assert_eq!(second.outcome.input_id, "u1");
    let inserted = [first.outcome.disposition, second.outcome.disposition]
        .into_iter()
        .filter(|disposition| *disposition == Disposition::Inserted)
        .count();
    assert_eq!(inserted, 1, "exactly one duplicate inserts the receipt");
    assert_eq!(
        [first.outcome.disposition, second.outcome.disposition]
            .into_iter()
            .filter(|disposition| *disposition == Disposition::Existing)
            .count(),
        1,
        "the other duplicate sees the receipt instead of dispatching again"
    );
    assert!(first.pending_turn.is_none() && second.pending_turn.is_none());
    assert_written(&store, &submission).await;
    assert_eq!(
        runtime.log.steer_generation(),
        1,
        "a same-id replay must not reserve a second generation"
    );
    assert!(runtime.log.steer_pending());
    assert_eq!(runtime.log.active_turn().as_deref(), Some("req-1"));
    assert_no_host_request(&server);
}

/// `NoTurn` 回退：准入 turn 在 handoff 前结束且没有替换者时，输入按普通 turn 受理——
/// 写 receipt 与 user item，把执行留给调用方（`pending_turn`），不占 generation、不派发。
#[tokio::test]
async fn a_steer_whose_turn_ends_before_the_handoff_becomes_a_pending_turn() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;

    let steer_guard = runtime.steer.lock().await;
    let submission = steer_submission("u1", "fallback");
    let accepting = spawn_steer(&service, submission.clone());
    wait_for_steer_lock(&runtime).await;
    assert!(runtime.admission.try_lock().is_ok());
    close_first_turn(&runtime).await;
    drop(steer_guard);

    let accepted = bounded(accepting, "the handoff falls back")
        .await
        .expect("accept task")
        .expect("a closed turn without a replacement falls back to a normal turn");
    assert_eq!(accepted.outcome.disposition, Disposition::Inserted);
    let pending = accepted
        .pending_turn
        .expect("the caller runs the fallback turn after 202");
    assert_eq!(pending.text, "fallback");
    assert_eq!(pending.logical_id, logical_id());
    assert_written(&store, &submission).await;
    assert_eq!(
        runtime.log.steer_generation(),
        0,
        "the fallback runs as a normal turn, not as a steer"
    );
    assert!(!runtime.log.steer_pending());
    assert_eq!(runtime.log.active_turn(), None);
    assert_no_host_request(&server);
}

/// `NoTurn` 回退与一个后继 turn 的启动竞争：测试持有 `admission`，调用方从 handoff 返回后
/// 只能停在回退的 admission 上；期间 `begin_turn("req-2")` 并发布 slot，再放锁。
/// 输入必须被拒绝且零写入（不派发、不写 receipt、不写 user item）。
#[tokio::test]
async fn a_steer_whose_turn_ends_while_a_replacement_starts_is_rejected_without_writing() {
    let Fixture {
        service,
        runtime,
        store,
        server,
        ..
    } = fixture(false).await;
    open_turn(&runtime, "req-1", "turn_1").await;

    let steer_guard = runtime.steer.lock().await;
    let submission = steer_submission("u1", "raced");
    let accepting = spawn_steer(&service, submission.clone());
    wait_for_steer_lock(&runtime).await;
    assert!(runtime.admission.try_lock().is_ok());
    close_first_turn(&runtime).await;

    // 先占住 admission：handoff 返回 `NoTurn` 后，调用方的回退只能停在这里。
    let admission = runtime.admission.lock().await;
    drop(steer_guard);
    // 取得 steer 锁证明调用方已把 guard 交给 handoff 任务且该任务已结束
    // （否则这里会与 handoff 竞争，恰好证明它还没返回）。
    let handoff_done = bounded(runtime.steer.lock(), "the handoff released the steer lock").await;
    drop(handoff_done);

    // 仍在 admission 内启动并发布替换 turn。
    open_turn(&runtime, "req-2", "turn_2").await;
    drop(admission);

    let error = bounded(
        accepting,
        "the fallback re-enters admission and is rejected",
    )
    .await
    .expect("accept task")
    .expect_err("a replacement turn owns the session");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert_eq!(error.details()["suggest"], "steer");
    assert_no_host_request(&server);
    assert_unwritten(&store, "u1").await;
    assert_eq!(runtime.log.steer_generation(), 0);
    assert!(!runtime.log.steer_pending());
    assert_eq!(runtime.log.active_turn().as_deref(), Some("req-2"));
}
