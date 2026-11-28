//! Swarm deployment with two PostgreSQL databases: the Peri session store and
//! the independent Swarm state store.
//!
//! Set `PERI_SWARM_SESSION_STORE` and `PERI_SWARM_STATE_STORE` to PostgreSQL
//! DSNs that name **different databases** (the same instance is fine). No Loom
//! service or credential is involved; the model endpoint is a deterministic
//! stub. Without the two variables the test prints one skip line and returns.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqlx::Row;
use tokio::sync::broadcast;

use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqEnqueueDisposition, MqFilter, MqInput,
};
use peri_acp_types::store::PersistedPayload;
use peri_swarm::actor::reader::TurnOutcome;
use peri_swarm::actor::runtime::{AcpChild, ChildConfig};
use peri_swarm::actor::ItemLog;
use peri_swarm::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, LogicalId, McpToolSpec,
    SessionState,
};
use peri_swarm::store::{
    InputPut, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmStateStore,
    SwarmStore, SWARM_STATE_SCHEMA_VERSION,
};

use crate::state_store_support::{
    assert_distinct_locators, assert_swarm_schema, clear_telemetry_env, close, connect,
    current_schema, declared_mcp_tools, deployment, environment, history_texts, machine_id, now,
    open, peri_schema, quote, required_env, settings_json, start_stub, state_store_config,
    wait_until, workspace, PERI_SCHEMA_VERSION, TURN_PREFIX,
};

// ── 1. two PostgreSQL databases ─────────────────────────────────────────────

/// `a` + `b` + `c` + `e`: the Peri session store carries the session/history/MQ
/// writes of a real turn, the independent Swarm state store carries the
/// session/item/input/binding rows, and both stay readable after the child and
/// every process connection are closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires two PostgreSQL databases"]
async fn postgres_carries_peri_session_history_mq_and_separate_swarm_state() {
    if std::env::var_os("SWARM_TRACE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    clear_telemetry_env();
    let Some((session_store_dsn, state_store_dsn)) = required_env() else {
        return;
    };
    assert_distinct_locators(&session_store_dsn, &state_store_dsn);

    let (base_url, stub) = start_stub().await;
    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);
    let store = deployment(&session_store_dsn);

    // Process-level deployment: Peri reads and MQ writes go to the session
    // database; Swarm rows go to the independent state database with its own
    // `swarm_meta` schema.
    let resources = open(&store, &env).await;

    // ── schema, before the state database exists in this process ────────────
    // The session database carries the Peri schema only, at the version this
    // build writes.
    let peri_probe = connect(&session_store_dsn).await;
    let peri_before = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_before.meta.len(),
        1,
        "peri_store_meta holds exactly one row: {:?}",
        peri_before.meta
    );
    assert_eq!(
        peri_before.meta[0].0, PERI_SCHEMA_VERSION,
        "the session database is at the Peri schema version: {:?}",
        peri_before.meta
    );
    assert!(
        peri_before
            .objects
            .iter()
            .all(|(_, name)| !name.starts_with("swarm_")),
        "the session database must carry no swarm table or index: {:?}",
        peri_before.objects
    );
    eprintln!(
        "state store: peri schema_version={} contract={} schema_objects={}",
        peri_before.meta[0].0,
        peri_before.meta[0].2,
        peri_before.objects.len()
    );

    // The state store opens a second, independent database. Its first open in a
    // fresh database creates the owned schema and pins `swarm_meta`.
    let state_store = Arc::new(
        SwarmStateStore::open(&state_store_config(&state_store_dsn))
            .await
            .expect("open the independent swarm state store"),
    );
    let swarm_probe = connect(&state_store_dsn).await;
    let swarm_schema = assert_swarm_schema(&swarm_probe).await;
    eprintln!(
        "state store: swarm schema_version={} objects={:?}",
        SWARM_STATE_SCHEMA_VERSION,
        swarm_schema
            .iter()
            .map(|(kind, name)| format!("{kind}:{name}"))
            .collect::<Vec<_>>()
    );

    let store_rows = Arc::clone(&state_store);
    let run = uuid::Uuid::now_v7().to_string();
    eprintln!("state store run id: {run}");
    let logical = LogicalId::parse(&format!("agent-state-child-{run}")).expect("logical id");
    let root_session_id = uuid::Uuid::now_v7().to_string();
    let created_at = now();
    let declared_mcp = declared_mcp_tools();
    let declared_mcp_json = McpToolSpec::encode_declaration(&declared_mcp)
        .expect("encode the canonical MCP declaration")
        .expect("a non-empty declaration has canonical JSON");

    // The child's item log writes into the **state** database: S4 broadcasts
    // one live delta per chunk but persists at most one row per item — the
    // assistant stream becomes a single `complete:true` row at the terminal.
    // `swarm_items` only repeats an id for legacy multi-row history.
    let item_store: Arc<dyn SwarmStore> = store_rows.clone();

    // The Swarm session row for the Peri root session: the same identity
    // `AcpSwarmService` writes before its log starts.
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: logical.as_str().to_owned(),
            root_session_id: root_session_id.clone(),
            actor_key: format!("agent:{}", logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Idle,
            model: Some("stub-model".to_owned()),
            instructions: Some("state store run".to_owned()),
            mcp_tools: declared_mcp.clone(),
            metadata: Default::default(),
            prompt_cache_options: None,
            environment: EnvironmentRef::none(),
            created_at: created_at.clone(),
            last_activity_at: created_at.clone(),
            last_item_seq: 0,
        })
        .await
        .expect("seed swarm session row");

    let log = Arc::new(ItemLog::new(
        logical.as_str(),
        item_store,
        broadcast::channel(256).0,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ));

    // ── a. real Peri host on the remote store, one turn ─────────────────────
    // Open the Swarm turn exactly like `AcpSwarmService::run_turn`: the log owns
    // `running`, the merged rows and the terminal.
    let request_id = "state-turn-1";
    log.begin_turn(request_id, "turn_state-1")
        .await
        .expect("open the swarm turn on the remote store");
    let child = AcpChild::spawn(
        ChildConfig {
            cwd: cwd.clone(),
            session_store: store.clone(),
            machine_id: Some(machine),
            workspace_root: Some(root.clone()),
            settings_json: Some(settings_json(&base_url)),
            model: None,
            instructions: Some("state store run".to_owned()),
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: root_session_id.clone(),
            session_resources: None,
        },
        Arc::clone(&log),
    )
    .await
    .expect("spawn the Peri host on the PostgreSQL session store");
    assert_eq!(
        child.session_id(),
        root_session_id,
        "the reserved root session id is the ACP session id"
    );

    let reply = child
        .prompt("hello postgres state", request_id)
        .await
        .expect("run one turn");
    // The production service closes the turn on the reader's terminal, not on
    // the prompt reply. Waiting for that terminal also drains items the reader
    // may still hold in its coalescing window, so the completion row is built
    // from the whole stream; the direct close is only the safety net.
    if !log
        .wait_turn_closed(request_id, Duration::from_secs(30))
        .await
    {
        log.finish_turn(
            request_id,
            TurnOutcome::Completed(
                reply
                    .stop_reason
                    .clone()
                    .expect("the host reports a terminal stop reason"),
            ),
        )
        .await;
    }
    assert_eq!(
        reply.stop_reason.as_deref(),
        Some("end_turn"),
        "turn reply: {reply:?}"
    );
    assert_eq!(
        stub.main_calls(),
        1,
        "one turn request (total requests incl. helpers: {})",
        stub.calls()
    );

    // The turn is projected into `swarm_items` through the real state store:
    // the assistant stream is **one** row (the merged `complete:true` row at
    // the terminal), so no live delta reaches the database. Shape and the
    // cached projection are checked here, not assumed.
    let child_session_live = store_rows
        .swarm_get_session(logical.as_str())
        .await
        .expect("child swarm session after the turn")
        .expect("the child swarm session row exists");
    assert_eq!(
        child_session_live.state,
        SessionState::Idle,
        "a persisted turn ends idle; `failed` would mean an append conflict"
    );
    let child_items = store_rows
        .swarm_items_after(logical.as_str(), 0, 100)
        .await
        .expect("child swarm items after the turn");
    let seqs: Vec<u64> = child_items.iter().map(|row| row.seq).collect();
    assert_eq!(
        seqs.len(),
        1,
        "the answer is the turn's only persisted row (the prompt is direct): {child_items:?}"
    );
    assert_eq!(seqs.first(), Some(&1), "seq starts at 1: {seqs:?}");
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "seq is strictly increasing: {seqs:?}"
    );
    assert!(
        !child_items
            .iter()
            .any(|row| row.payload["delta"].as_bool() == Some(true)),
        "live deltas are never persisted: {child_items:?}"
    );
    let completes: Vec<_> = child_items
        .iter()
        .filter(|row| row.payload["complete"].as_bool() == Some(true))
        .collect();
    assert_eq!(
        completes.len(),
        1,
        "the answer is exactly one merged row for the turn: {child_items:?}"
    );
    let stream_id = completes[0].item_id.as_str();
    let answer_rows: Vec<_> = child_items
        .iter()
        .filter(|row| row.item_id == stream_id)
        .collect();
    assert_eq!(
        answer_rows.len(),
        1,
        "the assistant item owns exactly one row {stream_id}: {child_items:?}"
    );
    assert_eq!(
        completes[0].payload["text"].as_str(),
        Some(format!("{TURN_PREFIX}0").as_str()),
        "the complete row carries the whole accumulated text"
    );
    assert_eq!(
        child_session_live.last_item_seq,
        *seqs.last().expect("at least one row"),
        "the cached projection equals MAX(seq)"
    );

    // Independent SQL over the state database: the same turn rows exist there.
    let schema = current_schema(&swarm_probe).await;
    let item_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT seq, item_id, payload_json FROM {}.swarm_items WHERE logical_id = $1 ORDER BY seq",
        quote(&schema)
    )))
    .bind(logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("swarm_items rows");
    assert_eq!(
        item_rows.len(),
        child_items.len(),
        "the database holds exactly the rows the store read"
    );
    let mut seen_ids = std::collections::HashSet::new();
    for row in &item_rows {
        let item_id: String = row.get(1);
        assert!(
            seen_ids.insert(item_id.clone()),
            "one item owns one row; {item_id} repeated across rows"
        );
    }
    let payloads: Vec<String> = item_rows
        .iter()
        .map(|row| row.get::<String, _>(2))
        .collect();
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| payload.contains("\"complete\":true"))
            .count(),
        1,
        "the complete row is persisted in the state database: {payloads:?}"
    );
    assert!(
        !payloads
            .iter()
            .any(|payload| payload.contains("\"delta\":true")),
        "no live delta is persisted in the state database: {payloads:?}"
    );
    eprintln!(
        "state store: swarm item rows={} seqs={:?} item_id={stream_id}",
        item_rows.len(),
        item_rows
            .iter()
            .map(|row| row.get::<i64, _>(0))
            .collect::<Vec<_>>()
    );

    // `mcp_json` is the canonical declaration text, not a derived shape.
    let mcp_rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT mcp_json FROM {}.swarm_sessions WHERE logical_id = $1",
        quote(&schema)
    )))
    .bind(logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("mcp_json rows");
    assert_eq!(
        mcp_rows,
        vec![declared_mcp_json.clone()],
        "the state database persists the canonical MCP declaration verbatim"
    );
    assert_eq!(
        child_session_live.mcp_tools, declared_mcp,
        "the child session row reads its declaration back"
    );

    // Peri session + history, read through the session database.
    let reader = resources.session_resources();
    let session_id = root_session_id.clone();
    wait_until("Peri session history", Duration::from_secs(30), || {
        let reader = Arc::clone(&reader);
        let session_id = session_id.clone();
        async move {
            match reader.load_session_history(&session_id).await {
                Ok(history) => history_texts(&history)
                    .iter()
                    .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
                Err(_) => false,
            }
        }
    })
    .await;

    let meta = reader
        .load_session_meta(&root_session_id)
        .await
        .expect("Peri session metadata from PostgreSQL");
    assert_eq!(meta.id, root_session_id);
    assert_eq!(meta.cwd, cwd.to_string_lossy());
    let history = reader
        .load_session_history(&root_session_id)
        .await
        .expect("Peri history from PostgreSQL");
    let texts = history_texts(&history);
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
        "the user turn is persisted: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "ai" && text.contains(&format!("{TURN_PREFIX}0"))),
        "the assistant answer is persisted: {texts:?}"
    );
    assert!(
        meta.message_count >= 2,
        "session metadata counts the persisted messages: {}",
        meta.message_count
    );

    // Peri MQ: a durable row through the same remote store.
    let mq_id = MessageId::new();
    let mq_text = "state store durable mq row";
    let outcome = reader
        .mq_enqueue(
            &root_session_id,
            MqInput {
                id: mq_id,
                kind: MessageKind::Prompt,
                source: MessageSource::UserInput,
                payload: PersistedPayload::Message(BaseMessage::Human {
                    id: mq_id,
                    content: MessageContent::text(mq_text),
                }),
                policy: MessagePolicy::ensure_processing(),
                original_draft: Some(mq_text.to_owned()),
                plugin_selections: Vec::new(),
            },
        )
        .await
        .expect("MQ write through the remote store");
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
    assert_eq!(
        reader.mq_count(&root_session_id).await.expect("MQ count"),
        1,
        "one durable MQ row before close"
    );

    // ── b. Swarm rows through the process-level state store ─────────────────
    let rows_logical = LogicalId::parse(&format!("agent-state-rows-{run}")).expect("logical id");
    let binding_env_id = format!("logical-env-state-{run}");
    let rows_root_session = uuid::Uuid::now_v7().to_string();
    store_rows
        .swarm_upsert_session(SwarmSessionRow {
            logical_id: rows_logical.as_str().to_owned(),
            root_session_id: rows_root_session.clone(),
            actor_key: format!("agent:{}", rows_logical.as_str()),
            cwd: cwd.to_string_lossy().into_owned(),
            state: SessionState::Running,
            model: Some("stub-model".to_owned()),
            instructions: Some("state swarm rows".to_owned()),
            mcp_tools: declared_mcp.clone(),
            metadata: Default::default(),
            prompt_cache_options: None,
            environment: EnvironmentRef {
                kind: EnvironmentKind::None,
                id: Some(rows_logical.as_str().to_owned()),
            },
            created_at: created_at.clone(),
            last_activity_at: created_at.clone(),
            last_item_seq: 0,
        })
        .await
        .expect("write swarm session row");

    for seq in 1..=2u64 {
        store_rows
            .swarm_append_item(SwarmItemRow {
                logical_id: rows_logical.as_str().to_owned(),
                seq,
                item_id: format!("itim_state_{run}_{seq}"),
                kind: ItemKind::Message,
                source: ItemSource::root(),
                payload: json!({
                    "state": true,
                    "seq": seq,
                    "text": format!("swarm row {seq}"),
                    "complete": true,
                }),
                created_at: now(),
                truncated: false,
            })
            .await
            .expect("append swarm item row");
    }

    let input_row = SwarmInputRow {
        logical_id: rows_logical.as_str().to_owned(),
        input_id: format!("state-input-{run}"),
        fingerprint: "fp-state-1".to_owned(),
        state: InputState::Accepted,
        created_at: now(),
    };
    assert_eq!(
        store_rows
            .swarm_put_input(input_row.clone())
            .await
            .expect("write swarm input row"),
        InputPut::Inserted
    );

    let binding_row = SwarmBindingRow {
        logical_env_id: binding_env_id.clone(),
        logical_id: rows_logical.as_str().to_owned(),
        sandbox_id: "sandbox-state".to_owned(),
        workspace_path: cwd.to_string_lossy().into_owned(),
        sandbox_root: root.to_string_lossy().into_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-state".to_owned()),
        last_used_at: now(),
    };
    store_rows
        .swarm_put_binding(binding_row.clone())
        .await
        .expect("write swarm binding row");

    let rows_last_seq = store_rows
        .swarm_last_seq(rows_logical.as_str())
        .await
        .expect("last seq before close");
    assert_eq!(rows_last_seq, 2);
    // `swarm_sessions.last_item_seq` is the cached projection; the actor updates
    // it after appending (design §4.4-5), so the row is coherent here too.
    store_rows
        .swarm_update_session(peri_swarm::store::SwarmSessionUpdate {
            logical_id: rows_logical.as_str().to_owned(),
            state: SessionState::Running,
            last_item_seq: rows_last_seq,
            last_activity_at: now(),
        })
        .await
        .expect("project last_item_seq");

    // ── isolation: the state database changed, the session schema did not ────
    let peri_after_swarm = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_after_swarm, peri_before,
        "creating and writing the Swarm state database must leave the session \
         database's catalog and peri_store_meta untouched"
    );
    eprintln!(
        "state store: isolation OK; peri schema_version={} objects={}",
        peri_after_swarm.meta[0].0,
        peri_after_swarm.objects.len()
    );
    assert_eq!(
        assert_swarm_schema(&swarm_probe).await,
        swarm_schema,
        "writing rows does not change the state database's own objects"
    );

    // ── c. close the child and every deployment connection ──────────────────
    let report = child.shutdown().await;
    assert!(report.complete, "host shutdown report: {report:?}");
    drop(reader);
    drop(log);
    close(resources).await;
    state_store
        .close()
        .await
        .expect("close the swarm state store");

    // ── c. reopen both databases with the same DSNs ─────────────────────────
    let reopened = open(&store, &env).await;
    let reopened_state = Arc::new(
        SwarmStateStore::open(&state_store_config(&state_store_dsn))
            .await
            .expect("reopen the independent swarm state store"),
    );
    let reopened_swarm = Arc::clone(&reopened_state);
    let reopened_reader = reopened.session_resources();

    let meta = reopened_reader
        .load_session_meta(&root_session_id)
        .await
        .expect("Peri session metadata after reopen");
    assert_eq!(meta.id, root_session_id);
    assert_eq!(meta.cwd, cwd.to_string_lossy());
    let history = reopened_reader
        .load_session_history(&root_session_id)
        .await
        .expect("Peri history after reopen");
    let texts = history_texts(&history);
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "human" && text.contains("hello postgres state")),
        "user history survives the reopen: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(role, text)| role == "ai" && text.contains(&format!("{TURN_PREFIX}0"))),
        "assistant history survives the reopen: {texts:?}"
    );

    let mq_rows = reopened_reader
        .mq_query(&root_session_id, MqFilter::default())
        .await
        .expect("MQ rows after reopen");
    assert_eq!(mq_rows.len(), 1, "the durable MQ row survives the reopen");
    assert_eq!(mq_rows[0].id, mq_id);
    assert_eq!(
        mq_rows[0]
            .payload
            .as_message()
            .map(|message| message.content()),
        Some(mq_text.to_owned())
    );

    // Swarm rows: the child session with its merged answer row, and every row
    // written in `b`.
    let child_session = reopened_swarm
        .swarm_get_session(logical.as_str())
        .await
        .expect("child swarm session after reopen")
        .expect("the child swarm session row survives");
    assert_eq!(child_session.root_session_id, root_session_id);
    assert_eq!(child_session.state, SessionState::Idle);
    assert_eq!(
        child_session.instructions.as_deref(),
        Some("state store run")
    );
    assert_eq!(
        child_session.mcp_tools, declared_mcp,
        "the child session's mcp_json round-trips the canonical declaration"
    );
    let reopened_child_items = reopened_swarm
        .swarm_items_after(logical.as_str(), 0, 100)
        .await
        .expect("child swarm items after reopen");
    assert_eq!(
        reopened_child_items, child_items,
        "the merged rows survive the close/reopen unchanged"
    );
    assert_eq!(
        child_session.last_item_seq,
        *seqs.last().expect("at least one row"),
        "the reopened session still projects MAX(seq)"
    );

    let rows_session = reopened_swarm
        .swarm_get_session(rows_logical.as_str())
        .await
        .expect("swarm session after reopen")
        .expect("the swarm session row survives");
    assert_eq!(rows_session.root_session_id, rows_root_session);
    assert_eq!(rows_session.state, SessionState::Running);
    assert_eq!(
        rows_session.instructions.as_deref(),
        Some("state swarm rows")
    );
    assert_eq!(
        rows_session.mcp_tools, declared_mcp,
        "the second session's mcp_json survives the reopen"
    );
    let reopened_mcp: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT mcp_json FROM {}.swarm_sessions WHERE logical_id = $1",
        quote(&schema)
    )))
    .bind(rows_logical.as_str())
    .fetch_all(&swarm_probe)
    .await
    .expect("mcp_json rows");
    assert_eq!(
        reopened_mcp,
        vec![declared_mcp_json.clone()],
        "the persisted mcp_json is still the canonical declaration after reopen"
    );
    assert_eq!(
        reopened_swarm
            .swarm_last_seq(rows_logical.as_str())
            .await
            .expect("last seq after reopen"),
        rows_last_seq
    );
    let items = reopened_swarm
        .swarm_items_after(rows_logical.as_str(), 0, 100)
        .await
        .expect("swarm items after reopen");
    assert_eq!(items.len(), 2);
    assert_eq!(
        items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(items.iter().all(|item| item.payload["state"] == true));
    let newest = reopened_swarm
        .swarm_items_before(rows_logical.as_str(), None, 10)
        .await
        .expect("swarm items descending after reopen");
    assert_eq!(
        newest.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(
        reopened_swarm
            .swarm_get_input(rows_logical.as_str(), &input_row.input_id)
            .await
            .expect("swarm input after reopen"),
        Some(input_row)
    );
    assert_eq!(
        reopened_swarm
            .swarm_get_binding(&binding_env_id)
            .await
            .expect("swarm binding after reopen"),
        Some(binding_row)
    );

    // The reopened state store ran its schema check again; that must not touch
    // the session database either.
    let peri_after_reopen = peri_schema(&peri_probe).await;
    assert_eq!(
        peri_after_reopen, peri_before,
        "reopening both stores leaves the session database's schema untouched"
    );

    // `d`: seq keeps growing after the restart (the storage half of a gateway
    // restart with another input on the same session).
    let next = reopened_swarm
        .swarm_last_seq(rows_logical.as_str())
        .await
        .expect("last seq")
        + 1;
    reopened_swarm
        .swarm_append_item(SwarmItemRow {
            logical_id: rows_logical.as_str().to_owned(),
            seq: next,
            item_id: format!("itim_state_{run}_after_restart"),
            kind: ItemKind::Message,
            source: ItemSource::root(),
            payload: json!({"state": true, "text": "after restart", "complete": true}),
            created_at: now(),
            truncated: false,
        })
        .await
        .expect("append after reopen");
    assert_eq!(
        reopened_swarm
            .swarm_last_seq(rows_logical.as_str())
            .await
            .expect("last seq"),
        next
    );

    close(reopened).await;
    reopened_state
        .close()
        .await
        .expect("close the reopened swarm state store");
    peri_probe.close().await;
    swarm_probe.close().await;
    eprintln!("state store: PostgreSQL run OK");
}
