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

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures::StreamExt as _;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::time::timeout;

use peri_swarm::actor::service::{AcpSwarmService, ActorServiceConfig};
use peri_swarm::domain::{
    ActorKey, ContentBlock, Disposition, EnvironmentKind, EnvironmentRef, ErrorCode, ErrorKind,
    EventCursor, InputKind, InputMode, InputSubmission, ItemKind, ItemOrder, ItemsWindow,
    LogicalId, OfficialEvent, SessionState, StreamEvent, TurnEvent, TurnsWindow,
};
use peri_swarm::service::{CreateSessionCommand, SessionUpdateCommand, SwarmService, UpdateField};
use peri_swarm::store::{
    MemorySwarmStore, SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};

/// One chunk of assistant output; the stub splits it in two SSE frames.
const TURN_PREFIX: &str = "stub-";

// ── model stub ──────────────────────────────────────────────────────────────

struct StubModel {
    calls: AtomicUsize,
    main_calls: AtomicUsize,
    delay_ms: AtomicU64,
    /// Pause between the first content frame and the rest of a turn's answer.
    chunk_gap_ms: AtomicU64,
    bodies: Mutex<Vec<Value>>,
}

impl StubModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            main_calls: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(0),
            chunk_gap_ms: AtomicU64::new(0),
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

    /// Send the first content frame, then hold the stream open for `millis`.
    ///
    /// A test that resumes mid-turn needs a chunk that is *consumed* while the
    /// turn is still running; a whole-response delay only proves the request
    /// arrived.
    fn set_chunk_gap(&self, millis: u64) {
        self.chunk_gap_ms.store(millis, Ordering::SeqCst);
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
    let gap = stub.chunk_gap_ms.load(Ordering::SeqCst);
    if gap > 0 {
        return streaming_sse_response(&text, index, &format!("main-{index}"), gap);
    }
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
        for frame in sse_frames(text, index, tag) {
            sse.push_str(&frame);
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

/// One text chunk's SSE frames, in the wire shape Peri's OpenAI client expects.
fn sse_frames(text: &str, index: usize, tag: &str) -> Vec<String> {
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
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

/// First content frame now, the rest after `gap_ms`.
///
/// The pause sits between the two content frames, so the host ingests and
/// projects a chunk while the turn is still in flight.
fn streaming_sse_response(text: &str, index: usize, tag: &str, gap_ms: u64) -> Response {
    let mut frames = sse_frames(text, index, tag);
    frames.push("data: [DONE]\n\n".to_owned());
    let body = Body::from_stream(futures::stream::iter(frames.into_iter().enumerate()).then(
        move |(position, frame)| async move {
            if position == 1 {
                tokio::time::sleep(Duration::from_millis(gap_ms)).await;
            }
            Ok::<_, std::io::Error>(Bytes::from(frame))
        },
    ));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("streaming SSE response")
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
            // Live-only chunk: it has no `seq` and no row, so the item list
            // stays the persisted view of the turn.
            StreamEvent::Delta(_) => {}
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
            StreamEvent::Official(_) => {}
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
            sandbox: None,
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
            mcp_tools: Vec::new(),
            metadata: Default::default(),
            prompt_cache_options: Default::default(),
            initial_input: Some("hello".to_owned()),
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
            defer_initial_turn: false,
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
    // The accepted input is a single user frame; the host's echo is not
    // projected, so the log can be folded into the full conversation.
    let user_items: Vec<_> = first
        .items
        .iter()
        .filter(|item| item.payload["role"] == "user")
        .collect();
    assert_eq!(user_items.len(), 1, "one frame per accepted input");
    assert_eq!(user_items[0].payload["text"], json!("hello"));
    // The assistant stream is one merged row; its live deltas carry the same
    // identity but are not rows (see `StreamEvent::Delta`).
    let message_items: Vec<_> = first
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Message && item.payload["messageId"].is_string())
        .collect();
    assert_eq!(
        message_items.len(),
        1,
        "one persisted row per assistant stream"
    );
    assert_eq!(message_items[0].payload["complete"], json!(true));
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
                after_id: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items");
    assert!(!all.items.is_empty());
    assert_eq!(all.last_item_seq, all.items.last().expect("last item").seq);

    let (backlog, mut reconnected) = service
        .subscribe(&logical, EventCursor::START)
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
        .subscribe(&logical, EventCursor::after(midpoint))
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
                    after_id: None,
                    limit: 2,
                },
            )
            .await
            .expect("page");
        // `limit` counts items, so `items` (every row of the page's items) can
        // exceed it; `data` carries exactly one row per item.
        assert!(page.data.len() <= 2);
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
                            after_id: None,
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

#[ignore = "local ACP measurement; the production idle path uses session/prompt"]
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
        Arc::new(AtomicU64::new(0)),
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
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: uuid(40),
            session_resources: None,
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

    let reached_model = wait_for_main_calls(&stub, 1, Duration::from_secs(60)).await;
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
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let service = AcpSwarmService::new(
        Arc::clone(&store),
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
            sandbox: None,
        },
    );
    let logical = LogicalId::parse("agent-s4-no-model").expect("logical id");
    let error = service
        .create_session(CreateSessionCommand {
            logical_id: logical.clone(),
            cwd: tmp.path().to_path_buf(),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            metadata: Default::default(),
            prompt_cache_options: Default::default(),
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
            defer_initial_turn: false,
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
    // The identity row survives as `creating` so a retry keeps the reserved
    // root id; no host is live for it (`design §3`).
    let row = store
        .swarm_get_session(logical.as_str())
        .await
        .expect("read the row")
        .expect("the interrupted create keeps its row");
    assert_eq!(row.state, SessionState::Creating);
    assert!(!service.is_live(&logical));
    assert!(service.shutdown_all().await.is_empty());
}

// ── interrupted create: the row is written before the Peri session ─────────

/// Poll `turns.list` until the host's terminal is visible.
///
/// `session/prompt` can answer before the terminal notification is ingested,
/// so the terminal is asynchronous from the caller's point of view.
async fn wait_turn(
    service: &AcpSwarmService,
    logical: &LogicalId,
    turn_id: &str,
    status: &str,
) -> peri_swarm::domain::OfficialTurn {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let turns = service
            .list_turns(
                logical,
                TurnsWindow {
                    order: ItemOrder::Desc,
                    after: None,
                    limit: 10,
                },
            )
            .await
            .expect("turns");
        if let Some(turn) = turns.data.iter().find(|turn| turn.id == turn_id) {
            if turn.status == status {
                return turn.clone();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "turn {turn_id} did not reach {status}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn service_config(base_url: &str, tmp: &tempfile::TempDir) -> ActorServiceConfig {
    ActorServiceConfig {
        settings_json: Some(settings_json(base_url)),
        session_store_path: Some(session_store_path(tmp)),
        channel_capacity: 64,
        ..ActorServiceConfig::default()
    }
}

fn create_command(
    logical: &LogicalId,
    cwd: &std::path::Path,
    initial_input: Option<&str>,
) -> CreateSessionCommand {
    CreateSessionCommand {
        logical_id: logical.clone(),
        cwd: cwd.to_path_buf(),
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        metadata: Default::default(),
        prompt_cache_options: Default::default(),
        initial_input: initial_input.map(str::to_owned),
        environment: EnvironmentKind::None,
        environment_id: None,
        root_session_id: None,
        defer_initial_turn: false,
    }
}

/// A row a killed process left behind: identity persisted, no host ever live.
fn creating_row(logical: &LogicalId, cwd: &std::path::Path, root: &str) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: logical.as_str().to_owned(),
        root_session_id: root.to_owned(),
        actor_key: ActorKey::new(logical.clone()).storage_key(),
        cwd: cwd.to_string_lossy().into_owned(),
        state: SessionState::Creating,
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        metadata: Default::default(),
        prompt_cache_options: None,
        environment: EnvironmentRef {
            kind: EnvironmentKind::None,
            id: Some(logical.as_str().to_owned()),
        },
        created_at: "2026-11-20T00:00:00.000000Z".to_owned(),
        last_activity_at: "2026-11-20T00:00:00.000000Z".to_owned(),
        last_item_seq: 0,
    }
}

async fn items_of(service: &AcpSwarmService, logical: &LogicalId) -> Vec<(u64, String)> {
    service
        .list_items(
            logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items")
        .items
        .into_iter()
        .map(|item| (item.seq, item.item_id))
        .collect()
}

/// Wait until the turn terminal is persisted.
///
/// `create_session` with an initial input returns once the prompt reply is in;
/// the reader may still be appending the completion item and writing `idle`.
async fn wait_idle(service: &AcpSwarmService, logical: &LogicalId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = service
            .get_session(logical)
            .await
            .expect("session view")
            .state;
        if state == SessionState::Idle {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session did not settle to idle: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_create_returns_before_the_first_turn_executes() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-create-defer").expect("logical id");
    let (_, mut live) = service.open_stream(&logical).await.into_parts();

    let mut command = create_command(&logical, &cwd, Some("hello"));
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    assert!(created.created);
    let pending = created
        .pending_initial
        .clone()
        .expect("the first turn is deferred");
    assert_eq!(pending.text, "hello");
    assert_eq!(
        created.session.state,
        SessionState::Running,
        "acceptance marks the session running before the turn executes"
    );
    assert_eq!(
        created.session.last_item_seq, 1,
        "defer persisted only the user item"
    );
    assert_eq!(stub.main_calls(), 0, "the turn has not run");
    let before = items_of(&service, &logical).await;
    assert_eq!(before.len(), 1, "no assistant item yet");
    assert_eq!(before[0].0, 1, "only the user item is persisted");

    service
        .run_pending_turn(pending)
        .await
        .expect("run the deferred first turn");
    wait_idle(&service, &logical).await;
    assert_eq!(stub.main_calls(), 1, "one turn request after the response");
    let events = collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(
        matches!(events.terminal, Some(TurnEvent::Completed { .. })),
        "deferred turn terminal: {:?}",
        events.terminal
    );
    assert!(
        events
            .items
            .iter()
            .any(|item| item.payload["role"] == json!("user")),
        "the persisted user item reaches the stream"
    );
    assert!(
        events.items.iter().any(|item| {
            item.payload["role"] == json!("assistant") && item.payload["complete"] == json!(true)
        }),
        "the assistant completion reaches the stream"
    );
    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
}

/// The deferred first turn replays the official frames an SDK sees, in order.
///
/// [`collect_until_terminal`] drops `StreamEvent::Official`; this test keeps
/// them and asserts the SDK-visible sequence of the actor path itself (for the
/// HTTP-level counterpart see `streaming_test`, which drives the fake).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_first_turn_emits_the_official_frames_in_sdk_order() {
    clear_telemetry_env();
    let (base_url, _stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-create-official").expect("logical id");
    let (_, mut live) = service.open_stream(&logical).await.into_parts();

    let mut command = create_command(&logical, &cwd, Some("hello"));
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    let pending = created.pending_initial.expect("the first turn is deferred");
    service
        .run_pending_turn(pending)
        .await
        .expect("run the deferred first turn");

    // The subscription predates the session row, so it observes the whole
    // first turn; `agent.session.idle` closes it and any other official frame
    // is unexpected.
    let mut official: Vec<OfficialEvent> = Vec::new();
    while !official
        .last()
        .is_some_and(|event| matches!(event, OfficialEvent::SessionIdle { .. }))
    {
        let event = timeout(Duration::from_secs(30), live.recv())
            .await
            .expect("the first turn terminates")
            .expect("the subscription stays live");
        if let StreamEvent::Official(event) = event {
            official.push(event);
        }
    }
    let names: Vec<&str> = official.iter().map(OfficialEvent::event_type).collect();
    assert_eq!(
        names,
        [
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
    );
    let OfficialEvent::TurnCreated { turn_id, turn, .. } = &official[1] else {
        panic!("the first official frame opens the turn: {:?}", official[1]);
    };
    assert!(
        turn.subagent_id.is_none(),
        "a swarm turn belongs to the root agent: {turn:?}"
    );
    let items: Vec<(&str, String)> = official[3..5]
        .iter()
        .map(|event| match event {
            OfficialEvent::TurnItemAdded {
                turn_id: Some(item_turn),
                item,
                ..
            } => {
                assert_eq!(item_turn, turn_id, "the item names the created turn");
                (item.role.as_str(), item.turn_id.clone())
            }
            other => panic!("expected turn.item.added: {other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        vec![("user", turn_id.clone()), ("assistant", turn_id.clone())],
        "the user frame precedes the assistant completion"
    );
    let OfficialEvent::TurnCompleted {
        turn_id: completed_turn,
        ..
    } = &official[11]
    else {
        panic!("the terminal names the turn: {:?}", official[11]);
    };
    assert_eq!(completed_turn, turn_id, "completed closes the created turn");

    // ILOG-6: replay rebuilds the same official frames. Live chunk deltas and
    // the single replay delta deliberately differ in `event_id`, so identity
    // parity excludes only those delta frames.
    let replay = service
        .replay_after(&logical, EventCursor::START)
        .await
        .expect("replay the finished first turn");
    let replayed: Vec<OfficialEvent> = replay
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Official(event) => Some(event),
            _ => None,
        })
        .collect();
    let identities = |events: &[OfficialEvent]| {
        events
            .iter()
            .filter(|event| !matches!(event, OfficialEvent::TurnOutputTextDelta { .. }))
            .map(|event| (event.event_type(), event.event_id().to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        identities(&replayed),
        identities(&official),
        "replay names the same turns and items as the live stream"
    );
    let replay_deltas: Vec<_> = replayed
        .iter()
        .filter_map(|event| match event {
            OfficialEvent::TurnOutputTextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replay_deltas,
        [format!("{TURN_PREFIX}0").as_str()],
        "replay has no chunk boundaries: one delta carries the full text"
    );

    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
}

/// ILOG-2: a finished turn replays exactly the item frames a live subscriber saw.
///
/// The live stream additionally carries custom `turn` frames and seq-less
/// `delta` frames; neither is persisted, so the comparison is over
/// `StreamEvent::Item` and over official `(event_type, event_id)` pairs only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_matches_live_frames() {
    clear_telemetry_env();
    let (base_url, _stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-replay-live").expect("logical id");
    let (_, mut live) = service.open_stream(&logical).await.into_parts();

    let mut command = create_command(&logical, &cwd, Some("hello replay"));
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    let pending = created.pending_initial.expect("the first turn is deferred");
    service
        .run_pending_turn(pending)
        .await
        .expect("run the deferred first turn");

    let mut live_items = Vec::new();
    let mut live_official = Vec::new();
    let mut live_deltas = 0usize;
    while !live_official
        .last()
        .is_some_and(|event| matches!(event, OfficialEvent::SessionIdle { .. }))
    {
        let event = timeout(Duration::from_secs(30), live.recv())
            .await
            .expect("the first turn terminates")
            .expect("the subscription stays live");
        match event {
            StreamEvent::Item(item) => live_items.push(item),
            StreamEvent::Delta(delta) => {
                assert_eq!(
                    delta.payload["delta"],
                    json!(true),
                    "a live chunk carries the streaming shape: {delta:?}"
                );
                live_deltas += 1;
            }
            StreamEvent::Official(event) => live_official.push(event),
            // Custom turn frames have no persisted counterpart.
            StreamEvent::Turn(_) | StreamEvent::Gap { .. } => {}
        }
    }
    assert!(
        live_deltas >= 2,
        "the live stream carried the model chunks: {live_deltas}"
    );
    assert_eq!(
        completed_text(&TurnEvents {
            items: live_items.clone(),
            ..Default::default()
        })
        .first()
        .map(|(_, text)| text.as_str()),
        Some(format!("{TURN_PREFIX}0").as_str()),
        "the live item list ends with the merged answer"
    );

    let replay = service
        .replay_after(&logical, EventCursor::START)
        .await
        .expect("replay the finished turn");
    let replay_items: Vec<_> = replay
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item.clone()),
            _ => None,
        })
        .collect();
    let replay_official: Vec<_> = replay
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Official(event) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(
        live_items, replay_items,
        "every persisted item frame replays verbatim and in order"
    );
    let identities = |events: &[OfficialEvent]| {
        events
            .iter()
            .filter(|event| !matches!(event, OfficialEvent::TurnOutputTextDelta { .. }))
            .map(|event| (event.event_type(), event.event_id().to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        identities(&live_official),
        identities(&replay_official),
        "the official frames replay with the same ids, in the same order"
    );
    let replay_deltas: Vec<_> = replay_official
        .iter()
        .filter_map(|event| match event {
            OfficialEvent::TurnOutputTextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replay_deltas,
        [format!("{TURN_PREFIX}0").as_str()],
        "replay sends one full-text delta, not the live chunk boundaries"
    );
}

/// ILOG-3: mid-turn replay exposes no half-written assistant row; after the
/// terminal the same cursor returns the merged row and the terminal exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_mid_turn_gets_final_row_once() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    // Send one chunk, then hold the rest of the answer: the mid-turn replay
    // below runs after a chunk was consumed but before the terminal.
    stub.set_chunk_gap(2_000);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = Arc::new(AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    ));
    let logical = LogicalId::parse("agent-reconnect-mid-turn").expect("logical id");

    let mut command = create_command(&logical, &cwd, Some("hello mid turn"));
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    let pending = created.pending_initial.expect("the first turn is deferred");
    let admitted = items_of(&service, &logical).await;
    assert_eq!(
        admitted.len(),
        1,
        "only the user item is durable: {admitted:?}"
    );
    let user_seq = admitted[0].0;

    let (_, mut live) = service
        .subscribe(&logical, EventCursor::Tail)
        .await
        .expect("live subscription")
        .into_parts();
    let runner = Arc::clone(&service);
    let turn = tokio::spawn(async move { runner.run_pending_turn(pending).await });
    assert!(
        wait_for_main_calls(&stub, 1, Duration::from_secs(10)).await,
        "the turn reaches the model"
    );
    // Wait until a live chunk was projected and broadcast: this is the event
    // that a per-chunk persistence would have written as a row.
    let delta = timeout(Duration::from_secs(10), async {
        loop {
            match live.recv().await.expect("the live subscription stays open") {
                StreamEvent::Delta(delta) => break delta,
                // The reader may emit unrelated live frames first (turn
                // markers, official frames); keep waiting for the chunk.
                _ => continue,
            }
        }
    })
    .await
    .expect("the first chunk is consumed before the replay");
    assert_eq!(delta.payload["delta"], json!(true));

    // Mid-turn: a cursor after the user item replays no item row at all, so a
    // reconnecting client cannot project a partial assistant message.
    let mid = service
        .replay_after(&logical, EventCursor::after(user_seq))
        .await
        .expect("mid-turn replay");
    assert!(
        mid.iter()
            .all(|event| !matches!(event, StreamEvent::Item(_))),
        "no item row exists before the turn terminal: {mid:?}"
    );

    turn.await
        .expect("the turn task finishes")
        .expect("the deferred turn completes");
    wait_idle(&service, &logical).await;

    let done = service
        .replay_after(&logical, EventCursor::after(user_seq))
        .await
        .expect("post-terminal replay from the same cursor");
    let items: Vec<_> = done
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(
        items.len(),
        1,
        "the answer is one merged row and appears once: {done:?}"
    );
    assert_eq!(items[0].seq, user_seq + 1, "the row follows the user item");
    assert_eq!(items[0].payload["complete"], json!(true));
    assert_eq!(
        items[0].payload["text"].as_str(),
        Some(format!("{TURN_PREFIX}0").as_str())
    );
    let types: Vec<&str> = done
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(event) => Some(event.event_type()),
            _ => None,
        })
        .collect();
    assert_eq!(
        types
            .iter()
            .filter(|event_type| **event_type == "agent.session.turn.completed")
            .count(),
        1,
        "one terminal per turn: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|event_type| **event_type == "agent.session.idle")
            .count(),
        1,
        "one idle frame per turn: {types:?}"
    );
    let seqs: Vec<u64> = done.iter().filter_map(StreamEvent::item_seq).collect();
    assert_eq!(
        seqs,
        vec![items[0].seq],
        "the replay carries no repeated seq: {done:?}"
    );
}

/// A create that replays its input into a reserved turn is refused before it
/// records anything.
///
/// The accepted turn's user item opens the log's turn slot; execution has not
/// started yet (the handler would spawn it). Recording the create input there
/// used to leave a stray opener whose turn could never start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_on_a_reserved_turn_is_refused_before_the_user_item() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-create-reserved").expect("logical id");
    service
        .create_session(create_command(&logical, &cwd, None))
        .await
        .expect("create");

    let accepted = service
        .accept_input(&logical, user_input(&uuid(31), InputMode::Turn, "occupied"))
        .await
        .expect("accept");
    assert!(accepted.pending_turn.is_some(), "the turn is due");
    assert_eq!(stub.main_calls(), 0, "the accepted turn has not run");
    let before = items_of(&service, &logical).await;
    assert_eq!(before.len(), 1, "one user item opened the turn");

    // Both create modes follow the same admission rule: `defer` only changes
    // who executes the turn, not whether a second opener may be recorded.
    for defer in [false, true] {
        let mut command = create_command(&logical, &cwd, Some("replayed"));
        command.defer_initial_turn = defer;
        let error = service
            .create_session(command)
            .await
            .expect_err("a create must not open a second turn");
        assert_eq!(error.code(), ErrorCode::TurnInProgress);
        assert_eq!(error.details()["suggest"], json!("steer"));
    }

    let after = items_of(&service, &logical).await;
    assert_eq!(
        after, before,
        "a refused create leaves no stray user item: {after:?}"
    );
    assert_eq!(stub.main_calls(), 0, "no turn was started");

    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
}

/// A create while the turn slot is held by a running prompt is refused too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_on_a_running_turn_is_refused_before_the_user_item() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    stub.set_delay(3_000);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = Arc::new(AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    ));
    let logical = LogicalId::parse("agent-create-running").expect("logical id");
    service
        .create_session(create_command(&logical, &cwd, None))
        .await
        .expect("create");
    let accepted = service
        .accept_input(
            &logical,
            user_input(&uuid(41), InputMode::Turn, "long task"),
        )
        .await
        .expect("accept");
    let pending = accepted.pending_turn.expect("the turn is due");
    let runner = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.run_pending_turn(pending).await })
    };
    // The model request proves the prompt is in flight and holds the slot.
    assert!(
        wait_for_main_calls(&stub, 1, Duration::from_secs(10)).await,
        "the running turn reaches the model"
    );

    for defer in [false, true] {
        let mut command = create_command(&logical, &cwd, Some("replayed"));
        command.defer_initial_turn = defer;
        let error = service
            .create_session(command)
            .await
            .expect_err("a create must not replay its input into a running turn");
        assert_eq!(error.code(), ErrorCode::TurnInProgress);
    }

    runner
        .await
        .expect("turn task")
        .expect("the accepted turn completes");
    wait_idle(&service, &logical).await;
    let page = service
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items");
    let user_texts: Vec<&str> = page
        .items
        .iter()
        .filter(|item| item.payload["role"] == json!("user"))
        .map(|item| item.payload["text"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        user_texts,
        ["long task"],
        "the refused creates recorded no user item"
    );

    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
}

/// A retried create on an existing idle row defers its first turn once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_idle_row_defers_the_first_turn_exactly_once() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-create-existing-defer").expect("logical id");
    let first = service
        .create_session(create_command(&logical, &cwd, None))
        .await
        .expect("create");
    assert!(first.created);

    let mut command = create_command(&logical, &cwd, Some("hello"));
    command.defer_initial_turn = true;
    let reused = service.create_session(command).await.expect("reuse");
    assert!(!reused.created, "an existing row is not a new session");
    let pending = reused.pending_initial.expect("the first turn is deferred");
    assert_eq!(
        reused.session.last_item_seq, 1,
        "defer persisted only the user item"
    );
    assert_eq!(reused.session.state, SessionState::Running);
    assert_eq!(stub.main_calls(), 0, "the deferred turn has not run");
    let before = items_of(&service, &logical).await;
    assert_eq!(before.len(), 1, "one user item, no assistant item");

    service
        .run_pending_turn(pending)
        .await
        .expect("run the deferred first turn");
    wait_idle(&service, &logical).await;
    assert_eq!(stub.main_calls(), 1, "one turn request after the reuse");
    let page = service
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: ItemsWindow::MAX_LIMIT,
            },
        )
        .await
        .expect("items");
    let user_items = page
        .items
        .iter()
        .filter(|item| item.payload["role"] == json!("user"))
        .count();
    assert_eq!(
        user_items, 1,
        "the deferred execution reuses the recorded item"
    );
    assert!(
        page.items.iter().any(|item| {
            item.payload["role"] == json!("assistant") && item.payload["complete"] == json!(true)
        }),
        "the reused row ran the assistant turn"
    );

    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creating_row_resumes_with_the_persisted_root_session_id() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("agent-create-recover").expect("logical id");
    let reserved = uuid(901);
    store
        .swarm_upsert_session(creating_row(&logical, &cwd, &reserved))
        .await
        .expect("seed the interrupted create row");

    let service = AcpSwarmService::new(Arc::clone(&store), service_config(&base_url, &tmp));
    let resumed = service
        .create_session(create_command(&logical, &cwd, None))
        .await
        .expect("resume the interrupted create");

    assert!(
        !resumed.created,
        "a recovered create reports the session, not a new one"
    );
    assert_eq!(resumed.session.root_session_id, reserved);
    assert_eq!(resumed.session.state, SessionState::Idle);
    assert!(service.is_live(&logical), "recovery starts one live host");
    let history = service
        .load_history(&logical, None, None)
        .await
        .expect("the peri session is readable");
    assert!(
        history.snapshot_token.is_some(),
        "session/load addressed the recovered peri session"
    );
    assert_eq!(stub.main_calls(), 0, "recovery prompts no turn");
    let reports = service.shutdown_all().await;
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].complete,
        "host shutdown report: {:?}",
        reports[0]
    );
    assert_eq!(
        reports[0].session_id, reserved,
        "the host ran the persisted root id"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creating_row_with_a_persisted_peri_session_reattaches_without_replaying() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("agent-create-reattach").expect("logical id");
    let command = create_command(&logical, &cwd, Some("hello"));

    let first = AcpSwarmService::new(Arc::clone(&store), service_config(&base_url, &tmp));
    let created = first
        .create_session(command.clone())
        .await
        .expect("first create");
    assert!(created.created);
    assert_eq!(stub.main_calls(), 1);
    let reserved = created.session.root_session_id.clone();
    wait_idle(&first, &logical).await;
    let before = items_of(&first, &logical).await;
    assert!(!before.is_empty(), "the first turn persisted items");
    let item_seq = first
        .get_session(&logical)
        .await
        .expect("view")
        .last_item_seq;
    assert!(first.shutdown_all().await[0].complete, "host died");

    // The process died after the row and the Peri session existed but before
    // the row reached `idle`; the retry sees exactly that state.
    store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: logical.as_str().to_owned(),
            state: SessionState::Creating,
            last_item_seq: item_seq,
            last_activity_at: "2026-11-20T00:00:00.000000Z".to_owned(),
        })
        .await
        .expect("mark the create as interrupted");

    let second = AcpSwarmService::new(Arc::clone(&store), service_config(&base_url, &tmp));
    // `stream:true` retries with the same deferred-create flag; the recovery
    // window must not turn the already persisted input into a second turn.
    let mut retry = command.clone();
    retry.defer_initial_turn = true;
    let resumed = second
        .create_session(retry)
        .await
        .expect("reattach to the persisted session");

    assert!(!resumed.created);
    assert!(
        resumed.pending_initial.is_none(),
        "a persisted item means the interrupted attempt already recorded the input"
    );
    assert_eq!(resumed.session.root_session_id, reserved);
    assert_eq!(resumed.session.state, SessionState::Idle);
    assert!(second.is_live(&logical), "the retry attached a host");
    assert_eq!(
        stub.main_calls(),
        1,
        "the recovered create does not replay the initial input"
    );
    assert_eq!(
        items_of(&second, &logical).await,
        before,
        "session/load replay does not duplicate persisted items"
    );
    assert_eq!(second.shutdown_all().await[0].session_id, reserved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retried_create_keeps_one_row_and_the_first_root_session_id() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("agent-create-retry").expect("logical id");
    let service = AcpSwarmService::new(Arc::clone(&store), service_config(&base_url, &tmp));

    let created = service
        .create_session(create_command(&logical, &cwd, None))
        .await
        .expect("first create");
    let reserved = created.session.root_session_id.clone();

    // A retry carrying a different reservation must not replace the identity
    // the store already holds.
    let mut retry = create_command(&logical, &cwd, None);
    retry.root_session_id = Some(uuid(903));
    let retried = service.create_session(retry).await.expect("retried create");
    assert!(!retried.created);
    assert_eq!(retried.session.root_session_id, reserved);

    let row = store
        .swarm_get_session(logical.as_str())
        .await
        .expect("read the row")
        .expect("one row");
    assert_eq!(row.logical_id, logical.as_str());
    assert_eq!(row.root_session_id, reserved);
    assert_eq!(row.state, SessionState::Idle);
    assert_eq!(
        stub.main_calls(),
        0,
        "neither create carried an initial input"
    );
    assert_eq!(service.shutdown_all().await[0].session_id, reserved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_create_keeps_the_creating_row_and_retries_with_the_same_root_id() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("agent-create-failure").expect("logical id");
    let reserved = uuid(904);

    // The deployment has no provider: the row is written, then assembly fails.
    let failing = AcpSwarmService::new(
        Arc::clone(&store),
        ActorServiceConfig {
            settings_json: Some(json!({ "config": {} }).to_string()),
            ..service_config(&base_url, &tmp)
        },
    );
    let mut command = create_command(&logical, &cwd, Some("hello"));
    command.root_session_id = Some(reserved.clone());
    let error = failing
        .create_session(command)
        .await
        .expect_err("the host cannot be assembled");
    assert_eq!(error.kind(), peri_swarm::domain::ErrorKind::BadGateway);
    assert!(!failing.is_live(&logical));

    let row = store
        .swarm_get_session(logical.as_str())
        .await
        .expect("read the row")
        .expect("the failed create keeps its row");
    assert_eq!(row.state, SessionState::Creating);
    assert_eq!(row.root_session_id, reserved);

    let service = AcpSwarmService::new(Arc::clone(&store), service_config(&base_url, &tmp));
    let recovered = service
        .create_session(create_command(&logical, &cwd, Some("hello")))
        .await
        .expect("the retry completes the create");
    assert!(!recovered.created);
    assert_eq!(recovered.session.root_session_id, reserved);
    wait_idle(&service, &logical).await;
    let view = service.get_session(&logical).await.expect("view");
    assert_eq!(view.state, SessionState::Idle);
    assert_eq!(
        stub.main_calls(),
        1,
        "the initial input runs once, on the attempt that reached a host"
    );
    let row = store
        .swarm_get_session(logical.as_str())
        .await
        .expect("read the row")
        .expect("still one row");
    assert_eq!(row.root_session_id, reserved);
    assert_eq!(row.state, SessionState::Idle);
    assert_eq!(service.shutdown_all().await[0].session_id, reserved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_rejects_a_root_session_id_the_host_could_never_honor() {
    clear_telemetry_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let service = AcpSwarmService::new(
        Arc::clone(&store),
        ActorServiceConfig {
            session_store_path: Some(session_store_path(&tmp)),
            channel_capacity: 16,
            ..ActorServiceConfig::default()
        },
    );
    let logical = LogicalId::parse("agent-create-bad-root").expect("logical id");
    let mut command = create_command(&logical, tmp.path(), None);
    command.root_session_id = Some("not-a-uuid".to_owned());

    let error = service
        .create_session(command)
        .await
        .expect_err("a non-UUIDv7 root id is refused");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(
        error.details()["field"],
        json!("rootSessionId"),
        "the refusal names the field: {}",
        error.details()
    );
    assert!(
        store
            .swarm_get_session(logical.as_str())
            .await
            .expect("read the row")
            .is_none(),
        "an identity the host can never honor is not persisted"
    );
}

/// The actor `GET /events` sequence for `POST /sessions?stream=true`:
/// `open_stream` (no row yet) → `replay_after(Tail)` → `create_session` runs the
/// first turn. The tail replay must not read the session row: doing so answered
/// `503 unknown_agent` and the create response lost every first-turn event
/// (pre-existing since `05d93fe1e`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tail_subscription_before_the_row_keeps_the_first_turn() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let service = AcpSwarmService::new(
        Arc::new(MemorySwarmStore::new()),
        service_config(&base_url, &tmp),
    );
    let logical = LogicalId::parse("agent-create-stream").expect("logical id");

    // No row exists yet: a tail subscription still gets an empty backlog.
    let backlog = service
        .replay_after(&logical, EventCursor::Tail)
        .await
        .expect("a tail replay must not require the session row");
    assert!(backlog.is_empty());
    // An explicit cursor still needs a known session.
    let error = service
        .replay_after(&logical, EventCursor::START)
        .await
        .expect_err("cursor replay requires the row");
    assert_eq!(error.code(), ErrorCode::UnknownAgent);

    // The actor endpoint registers the live receiver before replaying.
    let (_, mut live) = service.open_stream(&logical).await.into_parts();
    let backlog = service
        .replay_after(&logical, EventCursor::Tail)
        .await
        .expect("tail replay after subscribing");
    assert!(backlog.is_empty());

    service
        .create_session(create_command(&logical, &cwd, Some("hello")))
        .await
        .expect("create session");
    let first = collect_until_terminal(&mut live, Duration::from_secs(30)).await;
    assert!(
        matches!(&first.terminal, Some(TurnEvent::Completed { .. })),
        "first turn terminal: {:?}",
        first.terminal
    );
    assert!(
        first
            .items
            .iter()
            .any(|item| item.payload["role"] == "user"),
        "the accepted input reached the stream: {:?}",
        first.items
    );
    assert_eq!(
        completed_text(&first)
            .first()
            .map(|(_, text)| text.as_str()),
        Some(format!("{TURN_PREFIX}0").as_str())
    );
    assert_eq!(stub.main_calls(), 1, "one model call for the first turn");
}

// ── official session resources on the real in-process host ─────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turns_update_and_delete_round_trip_through_the_actor_service() {
    clear_telemetry_env();
    let (base_url, _stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(MemorySwarmStore::new());
    let store_port: Arc<dyn SwarmStore> = Arc::clone(&store) as Arc<dyn SwarmStore>;
    let service = AcpSwarmService::new(store_port, service_config(&base_url, &tmp));
    let logical = LogicalId::parse("agent-resources").expect("logical id");
    let created = service
        .create_session(create_command(&logical, tmp.path(), None))
        .await
        .expect("create");
    assert!(created.created);
    let first_root = created.session.root_session_id.clone();

    service
        .submit_input(
            &logical,
            user_input(
                "0192f0c5-0000-7000-8000-0000000000a1",
                InputMode::Turn,
                "hello",
            ),
        )
        .await
        .expect("turn");

    // `session/prompt` may answer before the host's terminal notification, so
    // wait for the reader to close the turn instead of assuming the reply is
    // the terminal.
    let turn_id = "turn_input_0192f0c5-0000-7000-8000-0000000000a1";
    wait_turn(&service, &logical, turn_id, "completed").await;
    let turns = service
        .list_turns(
            &logical,
            TurnsWindow {
                order: ItemOrder::Desc,
                after: None,
                limit: 10,
            },
        )
        .await
        .expect("turns");
    assert_eq!(turns.data.len(), 1, "{turns:?}");
    let turn = &turns.data[0];
    assert_eq!(turn.id, "turn_input_0192f0c5-0000-7000-8000-0000000000a1");
    assert_eq!(turn.status, "completed", "{turns:?}");
    assert!(turn.started_at.is_some());
    assert!(turn.completed_at.is_some());
    assert_eq!(turn.usage, None);
    assert_eq!(turn.subagent_id, None);

    let updated = service
        .update_session(
            &logical,
            SessionUpdateCommand {
                model: None,
                mcp_tools: None,
                metadata: peri_swarm::service::UpdateField::Set(
                    [("team".to_owned(), "swarm".to_owned())]
                        .into_iter()
                        .collect(),
                ),
                prompt_cache_options: peri_swarm::service::UpdateField::Set(
                    peri_swarm::domain::PromptCacheOptions::new(
                        peri_swarm::domain::PromptCacheTtl::TwelveHours,
                    ),
                ),
            },
        )
        .await
        .expect("update");
    assert_eq!(
        updated.metadata.get("team").map(String::as_str),
        Some("swarm")
    );
    assert_eq!(
        updated.prompt_cache_options.map(|options| options.ttl),
        Some(peri_swarm::domain::PromptCacheTtl::TwelveHours)
    );

    service.delete_session(&logical).await.expect("delete");
    assert!(!service.is_live(&logical), "the live host must be stopped");
    let missing = service
        .get_turn(&logical, &turn.id)
        .await
        .expect_err("deleted session");
    assert_eq!(missing.code(), ErrorCode::UnknownAgent);

    // Recreate with the same logical id: the actor memory and every store row
    // were invalidated, so this is a genuinely fresh session.
    let recreated = service
        .create_session(create_command(&logical, tmp.path(), None))
        .await
        .expect("recreate");
    assert!(recreated.created);
    assert_ne!(recreated.session.root_session_id, first_root);
    assert!(service
        .list_items(
            &logical,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 10,
            }
        )
        .await
        .expect("items")
        .items
        .is_empty());
    assert!(service
        .list_turns(
            &logical,
            TurnsWindow {
                order: ItemOrder::Desc,
                after: None,
                limit: 10,
            },
        )
        .await
        .expect("turns")
        .data
        .is_empty());

    service.shutdown_all().await;
}

#[tokio::test]
async fn a_persisted_unfinished_turn_refuses_update_and_delete_without_a_runtime() {
    let store = Arc::new(MemorySwarmStore::new());
    let store_port: Arc<dyn SwarmStore> = Arc::clone(&store) as Arc<dyn SwarmStore>;
    let service = AcpSwarmService::new(store_port, ActorServiceConfig::default());

    // `running` is the ordinary crash window; an `idle` row with an open turn
    // row is the case the session state alone cannot see.
    for (index, (logical, state)) in [
        ("agent-stale-running", SessionState::Running),
        ("agent-stale-idle", SessionState::Idle),
    ]
    .into_iter()
    .enumerate()
    {
        let logical = LogicalId::parse(logical).expect("logical id");
        let mut row = creating_row(
            &logical,
            std::path::Path::new("/tmp"),
            &format!("0192f0c5-0000-7000-8000-0000000000c{index}"),
        );
        row.state = state;
        store.swarm_upsert_session(row).await.expect("session");
        let turn_id = format!("turn_input_stale_{index}");
        store
            .swarm_upsert_turn(SwarmTurnRow {
                logical_id: logical.as_str().to_owned(),
                turn_id: turn_id.clone(),
                status: peri_swarm::domain::TurnStatus::InProgress,
                created_at: 100,
                started_at: Some(100),
                completed_at: None,
                error: None,
                usage: None,
                subagent_id: None,
            })
            .await
            .expect("turn");

        let update = service
            .update_session(
                &logical,
                SessionUpdateCommand {
                    metadata: UpdateField::Set(
                        [("attempt".to_owned(), "mutated".to_owned())]
                            .into_iter()
                            .collect(),
                    ),
                    ..SessionUpdateCommand::default()
                },
            )
            .await
            .expect_err("a persisted open turn is not updated");
        assert_eq!(update.code(), ErrorCode::UpstreamUnavailable);
        assert_eq!(update.kind(), ErrorKind::BadGateway);
        assert_eq!(
            update.details()["operation"],
            json!("session_resume_required")
        );

        let delete = service
            .delete_session(&logical)
            .await
            .expect_err("a persisted open turn is not deleted");
        assert_eq!(delete.code(), ErrorCode::UpstreamUnavailable);
        assert_eq!(delete.kind(), ErrorKind::BadGateway);
        assert_eq!(
            delete.details()["operation"],
            json!("session_resume_required")
        );

        // Neither refusal may mutate the row first: metadata is unchanged and
        // the turn is still open (not closed as `failed` by this path).
        let row = store
            .swarm_get_session(logical.as_str())
            .await
            .expect("read session")
            .expect("session");
        assert_eq!(row.state, state);
        assert!(row.metadata.is_empty());
        let turn = store
            .swarm_get_turn(logical.as_str(), &turn_id)
            .await
            .expect("read turn")
            .expect("turn");
        assert_eq!(turn.status, peri_swarm::domain::TurnStatus::InProgress);
    }
}

#[path = "actor_runtime/interrupted.rs"]
mod interrupted;
