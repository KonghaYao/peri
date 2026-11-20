//! S4 end-to-end: in-process ACP host + local OpenAI-compatible model stub.
//!
//! The test drives [`AcpSwarmService`] directly (no Rivet, no HTTP listener):
//! `session/new` with the reserved root session id → first turn → follow-up
//! turn → steering inside a running turn → cancel isolation → reconnect from
//! persisted history → item and ACP-history paging.
//!
//! The stub records every `/chat/completions` request and answers with a
//! deterministic SSE stream, so the assertions cover real host behavior while
//! the model itself stays a fake (`design §7.3`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::time::timeout;

use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::domain::{
    ContentBlock, Cursor, Disposition, EnvironmentKind, ErrorCode, InputKind, InputMode,
    InputSubmission, ItemKind, ItemOrder, ItemsWindow, LogicalId, SessionState, StreamEvent,
    TurnEvent,
};
use peri_swarm::service::{CreateSessionCommand, SwarmService};
use peri_swarm::store::MemorySwarmStore;

/// One chunk of assistant output; the stub splits it in two SSE frames.
const TURN_PREFIX: &str = "stub-";

// ── model stub ──────────────────────────────────────────────────────────────

struct StubModel {
    calls: AtomicUsize,
    main_calls: AtomicUsize,
    delay_ms: AtomicU64,
    bodies: Mutex<Vec<Value>>,
}

impl StubModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            main_calls: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(0),
            bodies: Mutex::new(Vec::new()),
        }
    }

    /// Every `/chat/completions` request, including Peri's background helpers.
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Requests that belong to a turn (background prediction helpers excluded).
    fn main_calls(&self) -> usize {
        self.main_calls.load(Ordering::SeqCst)
    }

    fn set_delay(&self, millis: u64) {
        self.delay_ms.store(millis, Ordering::SeqCst);
    }

    fn requests(&self) -> Vec<Value> {
        self.bodies.lock().clone()
    }

    /// Last user message text seen in any recorded request.
    fn delivered_user_texts(&self) -> Vec<String> {
        let bodies = self.bodies.lock();
        let mut texts = Vec::new();
        for body in bodies.iter() {
            let Some(messages) = body.get("messages").and_then(Value::as_array) else {
                continue;
            };
            for message in messages {
                if message.get("role").and_then(Value::as_str) == Some("user") {
                    let text = message
                        .get("content")
                        .map(message_content_text)
                        .unwrap_or_default();
                    if !text.is_empty() {
                        texts.push(text);
                    }
                }
            }
        }
        texts
    }
}

fn message_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

async fn start_stub() -> (String, Arc<StubModel>) {
    let stub = Arc::new(StubModel::new());
    let router = Router::new()
        .fallback(any(stub_chat))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/v1"), stub)
}

async fn stub_chat(State(stub): State<Arc<StubModel>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.calls.fetch_add(1, Ordering::SeqCst);
    if is_background_helper(&request) {
        // Peri's prediction/title helper runs beside the turn; it must not
        // shift the deterministic turn text or the recorded request counts.
        let delay = stub.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        return sse_response(&[format!("{TURN_PREFIX}helper")], 0, "helper");
    }
    // Record the request before sleeping: `main_calls` is the in-flight
    // signal the steer assertions wait for, and it must flip while the model
    // is still "thinking" rather than after the answer was produced.
    let index = stub.main_calls.fetch_add(1, Ordering::SeqCst);
    stub.bodies.lock().push(request);
    let delay = stub.delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let text = format!("{TURN_PREFIX}{index}");
    sse_response(&[text], index, &format!("main-{index}"))
}

/// Background helpers are recognisable by their dedicated system directive.
fn is_background_helper(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("content").is_some_and(|content| {
                    message_content_text(content).contains("<prediction_directive>")
                })
            })
        })
}

fn sse_response(texts: &[String], index: usize, tag: &str) -> Response {
    let mut sse = String::new();
    for text in texts {
        let (head, tail) = text.split_at(text.len().min(5));
        let frames = [
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": head } }],
            }),
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": { "content": tail } }],
            }),
            json!({
                "id": format!("chatcmpl-stub-{tag}-{index}"),
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            }),
        ];
        for frame in frames {
            sse.push_str("data: ");
            sse.push_str(&frame.to_string());
            sse.push_str("\n\n");
        }
    }
    sse.push_str("data: [DONE]\n\n");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        sse,
    )
        .into_response()
}

// ── harness ─────────────────────────────────────────────────────────────────

fn settings_json(base_url: &str) -> String {
    json!({
        "config": {
            "active_alias": "sonnet",
            "providers": [{
                "id": "local-stub",
                "type": "openai",
                "apiKey": "test-key-not-a-credential",
                "baseUrl": base_url,
            }],
            "profiles": {
                "sonnet": { "provider": "local-stub", "model": "stub-model" }
            }
        }
    })
    .to_string()
}

fn session_store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("threads.db")
}

/// Keep the run hermetic: the ACP host enables Langfuse whenever the deployment
/// sees both keys, so an inherited developer environment would make
/// `shutdown()` depend on a real endpoint (and ship this test's traffic there).
fn clear_telemetry_env() {
    for key in [
        "LANGFUSE_PUBLIC_KEY",
        "LANGFUSE_SECRET_KEY",
        "LANGFUSE_HOST",
        "LANGFUSE_BASE_URL",
        "LANGFUSE_USER_ID",
        "LANGFUSE_TRACE_SAMPLING",
    ] {
        std::env::remove_var(key);
    }
}

fn user_input(id: &str, mode: InputMode, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: id.to_owned(),
        kind: InputKind::User,
        mode,
        text: Some(text.to_owned()),
        attachments: Vec::<ContentBlock>::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

fn control_input(id: &str, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: id.to_owned(),
        kind: InputKind::Control,
        mode: InputMode::Turn,
        text: Some(text.to_owned()),
        attachments: Vec::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

fn uuid(index: u64) -> String {
    format!("0192f0c5-0000-7000-8000-{:012}", index)
}

async fn next_event(
    receiver: &mut broadcast::Receiver<StreamEvent>,
    within: Duration,
) -> Option<StreamEvent> {
    timeout(within, receiver.recv()).await.ok()?.ok()
}

#[derive(Default)]
struct TurnEvents {
    items: Vec<peri_swarm::domain::ItemView>,
    turns: Vec<TurnEvent>,
    terminal: Option<TurnEvent>,
}

async fn collect_until_terminal(
    receiver: &mut broadcast::Receiver<StreamEvent>,
    within: Duration,
) -> TurnEvents {
    let mut events = TurnEvents::default();
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        let Some(event) = next_event(receiver, Duration::from_secs(5)).await else {
            continue;
        };
        match event {
            StreamEvent::Item(item) => events.items.push(item),
            StreamEvent::Turn(turn) => {
                let terminal =
                    matches!(turn, TurnEvent::Completed { .. } | TurnEvent::Failed { .. });
                events.turns.push(turn.clone());
                if terminal {
                    events.terminal = Some(turn);
                    return events;
                }
            }
            StreamEvent::Gap { .. } => {}
        }
    }
    events
}

async fn wait_for_main_calls(stub: &StubModel, at_least: usize, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if stub.main_calls() >= at_least {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

fn completed_text(events: &TurnEvents) -> Vec<(String, String)> {
    events
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Message && item.payload["complete"] == true)
        .map(|item| {
            (
                item.item_id.clone(),
                item.payload["text"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_process_acp_session_projects_turns_steering_cancel_and_paging() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = Arc::new(AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        ActorServiceConfig {
            settings_json: Some(settings_json(&base_url)),
            session_store_path: Some(session_store_path(&tmp)),
            session_store: None,
            machine_id: None,
            workspace_root: None,
            model: None,
            channel_capacity: 512,
        },
    ));
    let logical = LogicalId::parse("agent-s4-e2e").expect("logical id");

    // Register the stream before the first turn executes (`open_stream`).
    let (_, mut live) = service.open_stream(&logical).await.into_parts();

    // ── create + first turn ────────────────────────────────────────────────
    let created = service
        .create_session(CreateSessionCommand {
            logical_id: logical.clone(),
            cwd: cwd.clone(),
            model: None,
            instructions: Some("keep it short".to_owned()),
            initial_input: Some("hello".to_owned()),
            environment: EnvironmentKind::None,
            root_session_id: None,
        })
        .await
        .expect("create session");
    assert!(created.created, "first create must create");
    assert!(
        matches!(
            created.session.state,
            SessionState::Running | SessionState::Idle
        ),
        "create with an initial input starts a turn: {:?}",
        created.session.state
    );
    assert_eq!(created.session.cwd, cwd.to_string_lossy());
    assert_eq!(
        created.session.agent.instructions.as_deref(),
        Some("keep it short")
    );
    let root_session_id = created.session.root_session_id.clone();
    let parsed = uuid::Uuid::parse_str(&root_session_id).expect("uuid root session id");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::SortRand),
        "reserved root session id must be UUIDv7"
    );
    assert_eq!(
        stub.main_calls(),
        1,
        "one turn request for the first turn (total requests incl. helpers: {})",
        stub.calls()
    );

    let first = collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(
        matches!(&first.terminal, Some(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "end_turn"),
        "first turn terminal: {:?}",
        first.terminal
    );
    let first_texts = completed_text(&first);
    assert_eq!(first_texts.len(), 1, "one completed message stream");
    assert_eq!(first_texts[0].1, format!("{TURN_PREFIX}0"));
    // `model`/`instructions` reach the provider through the assembled config.
    let recorded = stub.requests();
    assert_eq!(
        recorded[0].get("model").and_then(Value::as_str),
        Some("stub-model"),
        "the profile model is what the provider is asked for"
    );
    assert!(
        recorded[0]
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message_content_text(&message["content"]).contains("keep it short")
                })
            }),
        "inline instructions reach the model request: {}",
        recorded[0]
    );
    let idle = service.get_session(&logical).await.expect("session view");
    assert_eq!(idle.state, SessionState::Idle, "turn finished");
    // All deltas and the completion item of one message share one identity.
    let message_items: Vec<_> = first
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Message)
        .collect();
    assert!(message_items.len() >= 2, "deltas plus the completion item");
    let ids: HashSet<_> = message_items
        .iter()
        .map(|item| item.item_id.clone())
        .collect();
    assert_eq!(ids.len(), 1, "one stable item id per message");
    assert!(first
        .turns
        .iter()
        .any(|turn| matches!(turn, TurnEvent::Started { .. })));
    let seqs: Vec<u64> = first.items.iter().map(|item| item.seq).collect();
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "item seq is strictly increasing"
    );

    // ── follow-up turn while idle ──────────────────────────────────────────
    let follow = service
        .submit_input(&logical, user_input(&uuid(1), InputMode::Turn, "again"))
        .await
        .expect("follow-up accepted");
    assert_eq!(follow.disposition, Disposition::Inserted);
    assert_eq!(
        stub.main_calls(),
        2,
        "the follow-up opened a second model request"
    );
    let second = collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(matches!(second.terminal, Some(TurnEvent::Completed { .. })));
    assert_eq!(completed_text(&second)[0].1, format!("{TURN_PREFIX}1"));

    // ── steering inside a running turn ─────────────────────────────────────
    // Hold the in-flight answer so the steer lands while the turn is waiting on
    // the model: the host cancels that attempt and continues the handed-off
    // input as a queued run.
    stub.set_delay(3000);
    let turn = {
        let service = Arc::clone(&service);
        let logical = logical.clone();
        let submission = user_input(&uuid(2), InputMode::Turn, "running turn");
        tokio::spawn(async move { service.submit_input(&logical, submission).await })
    };
    assert!(
        wait_for_main_calls(&stub, 3, Duration::from_secs(15)).await,
        "third model request started"
    );
    let steer = service
        .submit_input(
            &logical,
            user_input(&uuid(3), InputMode::Steer, "steer-me-now"),
        )
        .await
        .expect("steer accepted while running");
    assert!(steer.accepted);
    let running = service.get_session(&logical).await.expect("session view");
    assert_eq!(
        running.state,
        SessionState::Running,
        "steering must not close the turn"
    );
    // The host cancels the in-flight attempt and continues the handoff as a
    // queued run; the same Swarm turn ends when that run terminates.
    assert!(
        wait_for_main_calls(&stub, 4, Duration::from_secs(30)).await,
        "the steer continuation reached the model (calls={}, main={}, bodies={})",
        stub.calls(),
        stub.main_calls(),
        stub.requests()
            .iter()
            .map(|body| body["messages"].to_string())
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
    let third = collect_until_terminal(&mut live, Duration::from_secs(60)).await;
    let turn_result = timeout(Duration::from_secs(60), turn)
        .await
        .expect("turn task finished")
        .expect("turn task joined")
        .expect("running turn accepted");
    assert_eq!(turn_result.input_id, uuid(2));
    assert!(
        matches!(
            &third.terminal,
            Some(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "end_turn"
        ),
        "the steered turn ends with the continuation, got {:?}",
        third.terminal
    );
    assert!(
        stub.delivered_user_texts()
            .iter()
            .any(|text| text.contains("steer-me-now")),
        "the steer input reached the model inside the running turn"
    );
    assert_eq!(
        third
            .turns
            .iter()
            .filter(|turn| matches!(turn, TurnEvent::Completed { .. }))
            .count(),
        1,
        "steering must not open a second Swarm turn"
    );
    assert_eq!(
        third
            .turns
            .iter()
            .filter(|turn| matches!(turn, TurnEvent::Started { .. }))
            .count(),
        1,
        "the queued run continues the open turn instead of starting one"
    );

    // A second `mode=turn` input while running is refused, not silently queued.
    stub.set_delay(400);
    let main_before = stub.main_calls();
    let running_turn = {
        let service = Arc::clone(&service);
        let logical = logical.clone();
        let submission = user_input(&uuid(4), InputMode::Turn, "will be cancelled");
        tokio::spawn(async move { service.submit_input(&logical, submission).await })
    };
    assert!(wait_for_main_calls(&stub, main_before + 1, Duration::from_secs(15)).await);
    let conflict = service
        .submit_input(
            &logical,
            user_input(&uuid(5), InputMode::Turn, "not allowed"),
        )
        .await
        .expect_err("second turn while running");
    assert_eq!(conflict.code(), ErrorCode::TurnInProgress);

    // ── cancel is bound to the observed turn ──────────────────────────────
    let cancel = service
        .submit_input(&logical, control_input(&uuid(7), "cancel"))
        .await
        .expect("cancel accepted");
    assert_eq!(cancel.mode, InputMode::Turn);
    let cancelled_events = collect_until_terminal(&mut live, Duration::from_secs(40)).await;
    let cancelled_terminal = cancelled_events
        .terminal
        .clone()
        .expect("cancelled turn terminal");
    assert!(
        matches!(&cancelled_terminal, TurnEvent::Completed { stop_reason, .. } if stop_reason == "cancelled")
            || matches!(&cancelled_terminal, TurnEvent::Failed { .. }),
        "cancel terminal: {cancelled_terminal:?}"
    );
    let _ = timeout(Duration::from_secs(40), running_turn)
        .await
        .expect("cancelled turn task finished");

    // A stale cancel while idle must not stop the next turn.
    let stale = service
        .submit_input(&logical, control_input(&uuid(8), "cancel"))
        .await
        .expect("stale cancel accepted");
    assert!(stale.accepted);
    stub.set_delay(0);
    let calls_before = stub.main_calls();
    let after_cancel = service
        .submit_input(
            &logical,
            user_input(&uuid(9), InputMode::Turn, "after cancel"),
        )
        .await
        .expect("next turn after cancel");
    assert!(after_cancel.accepted);
    let after = collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(
        matches!(&after.terminal, Some(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "end_turn"),
        "the turn after a stale cancel must run to completion, got {:?}",
        after.terminal
    );
    assert_eq!(
        stub.main_calls(),
        calls_before + 1,
        "exactly one more model call"
    );

    // ── reconnect from persisted history ─────────────────────────────────
    let all = service
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items");
    assert!(!all.items.is_empty());
    assert_eq!(all.last_item_seq, all.items.last().expect("last item").seq);

    let (backlog, mut reconnected) = service
        .subscribe(&logical, Cursor::START)
        .await
        .expect("subscribe")
        .into_parts();
    let replayed: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replayed.len(),
        all.items.len(),
        "reconnect replays every persisted item exactly once"
    );
    assert_eq!(
        replayed.iter().map(|item| item.seq).collect::<Vec<_>>(),
        all.items.iter().map(|item| item.seq).collect::<Vec<_>>()
    );

    let midpoint = all.items[all.items.len() / 2].seq;
    let (tail, _) = service
        .subscribe(&logical, Cursor::new(midpoint))
        .await
        .expect("subscribe tail")
        .into_parts();
    assert!(tail
        .iter()
        .all(|event| event.item_seq().is_none_or(|seq| seq > midpoint)));

    // Live delivery still works after a reconnect subscription.
    let before_live = stub.main_calls();
    let live_turn = service
        .submit_input(&logical, user_input(&uuid(10), InputMode::Turn, "live"))
        .await
        .expect("live turn");
    assert!(live_turn.accepted);
    let live_events = collect_until_terminal(&mut reconnected, Duration::from_secs(30)).await;
    assert!(matches!(
        live_events.terminal,
        Some(TurnEvent::Completed { .. })
    ));
    assert_eq!(stub.main_calls(), before_live + 1);

    // ── item paging ───────────────────────────────────────────────────────
    let mut paged = Vec::new();
    let mut after = None;
    loop {
        let page = service
            .list_items(
                &logical,
                ItemsWindow {
                    order: ItemOrder::Asc,
                    after,
                    limit: 2,
                },
            )
            .await
            .expect("page");
        assert!(page.items.len() <= 2);
        paged.extend(page.items.iter().map(|item| item.seq));
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(
        paged,
        all.items
            .iter()
            .map(|item| item.seq)
            .chain(
                service
                    .list_items(
                        &logical,
                        ItemsWindow {
                            order: ItemOrder::Asc,
                            after: Some(all.items.last().expect("last").seq),
                            limit: ItemsWindow::MAX_LIMIT,
                        },
                    )
                    .await
                    .expect("items after")
                    .items
                    .iter()
                    .map(|item| item.seq)
            )
            .collect::<Vec<_>>(),
        "paging visits every seq once, in order"
    );

    // ── ACP history paging (`session/load`) ───────────────────────────────
    let initial = service
        .load_history(&logical, None, None)
        .await
        .expect("initial history");
    assert!(
        initial.snapshot_token.is_some(),
        "negotiated history pagination returns a snapshot token"
    );
    if let Some(cursor) = initial.next_cursor.clone() {
        let page = service
            .load_history(&logical, Some(&cursor), Some(1))
            .await
            .expect("history page");
        assert_eq!(page.snapshot_token, initial.snapshot_token);
    }
    let limit_without_cursor = service
        .load_history(&logical, None, Some(1))
        .await
        .expect_err("limit without cursor");
    assert_eq!(limit_without_cursor.code(), ErrorCode::InvalidInput);
    let malformed = service
        .load_history(&logical, Some("not-a-cursor"), None)
        .await
        .expect_err("malformed cursor");
    assert_eq!(malformed.code(), ErrorCode::InvalidInput);

    // ── teardown ──────────────────────────────────────────────────────────
    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
    assert_eq!(reports[0].session_id, root_session_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_mq_dispatch_produces_a_turn() {
    // Measurement for `design §9 R2`: does `session/input/enqueue` +
    // `session/input/dispatch` reach the model while the session is idle?
    // The answer decides whether `mode=turn` can share the steer path.
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let logical = LogicalId::parse("agent-s4-idle-mq").expect("logical id");
    let log = Arc::new(peri_swarm::actor::ItemLog::new(
        logical.as_str(),
        Arc::new(MemorySwarmStore::new()),
        broadcast::channel(64).0,
    ));
    let child = peri_swarm::actor::AcpChild::spawn(
        peri_swarm::actor::ChildConfig {
            cwd: tmp.path().to_path_buf(),
            session_store: peri_swarm::actor::runtime::local_session_store(Some(
                session_store_path(&tmp),
            )),
            settings_json: Some(settings_json(&base_url)),
            machine_id: None,
            workspace_root: None,
            model: None,
            instructions: None,
            root_session_id: uuid(40),
        },
        Arc::clone(&log),
    )
    .await
    .expect("spawn child");

    let snapshot = child.snapshot().await.expect("snapshot");
    let generation = snapshot
        .get("generation")
        .and_then(Value::as_str)
        .expect("generation")
        .to_owned();
    child
        .enqueue_input(
            &uuid(41),
            &generation,
            &uuid::Uuid::now_v7().to_string(),
            "idle mq input",
        )
        .await
        .expect("enqueue");
    child
        .dispatch_inputs(&generation, &uuid::Uuid::now_v7().to_string(), &[uuid(41)])
        .await
        .expect("dispatch");

    let reached_model = wait_for_main_calls(&stub, 1, Duration::from_secs(20)).await;
    eprintln!(
        "IDLE_MQ reached_model={reached_model} calls={} main={}",
        stub.calls(),
        stub.main_calls()
    );
    let shutdown = child.shutdown().await;
    assert!(
        reached_model,
        "idle enqueue+dispatch must open a turn: calls={} main={}",
        stub.calls(),
        stub.main_calls()
    );
    assert!(shutdown.complete, "host shutdown: {shutdown:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_unavailable_is_a_diagnosable_error() {
    clear_telemetry_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        ActorServiceConfig {
            // A trusted document with no provider profile: assembly must fail
            // before a host is spawned, and the client must get a 502 that says
            // *what* is missing (never a credential value).
            settings_json: Some(json!({ "config": {} }).to_string()),
            session_store_path: Some(session_store_path(&tmp)),
            session_store: None,
            machine_id: None,
            workspace_root: None,
            model: None,
            channel_capacity: 16,
        },
    );
    let logical = LogicalId::parse("agent-s4-no-model").expect("logical id");
    let error = service
        .create_session(CreateSessionCommand {
            logical_id: logical.clone(),
            cwd: tmp.path().to_path_buf(),
            model: None,
            instructions: None,
            initial_input: None,
            environment: EnvironmentKind::None,
            root_session_id: None,
        })
        .await
        .expect_err("no provider");
    assert_eq!(error.code(), ErrorCode::AgentError);
    assert_eq!(error.kind(), peri_swarm::domain::ErrorKind::BadGateway);
    assert!(
        error.message().contains("no LLM provider"),
        "message: {}",
        error.message()
    );
    // No row and no runtime survive the failure (`design §7.4-1`).
    assert!(service.get_session(&logical).await.is_err());
    assert!(service.shutdown_all().await.is_empty());
}
