//! Tests for [`super::RemoteSwarmStore`] against a fake [`SwarmStorePort`].
//!
//! The fake stores **port** rows (not domain rows), so these tests fail if the
//! adapter drops or reshapes a field between the S3 row and the S6a port row.
//! Loom itself is not contacted: the real store is verified in S6b2.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_resources::{
    SwarmBindingRow as PortBindingRow, SwarmInputPut as PortInputPut,
    SwarmInputRow as PortInputRow, SwarmItemRow as PortItemRow, SwarmSessionRow as PortSessionRow,
    SwarmSessionUpdate as PortSessionUpdate, SwarmStorePort,
};
use serde_json::{json, Map};

use super::RemoteSwarmStore;
use crate::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, McpHeader, McpToolSpec,
    SessionState,
};
use crate::store::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};

// ── fake port ───────────────────────────────────────────────────────────────

/// Deterministic port failure injected into every call.
#[derive(Clone, Copy, Debug)]
enum Fault {
    Conflict,
    NotFound,
    Unsupported,
    ReadOnly,
    Uncertain,
    Internal,
}

impl Fault {
    fn error(self) -> SessionResourceError {
        let kind = match self {
            Self::Conflict => SessionResourceErrorKind::Conflict {
                detail: "unique key".to_owned(),
            },
            Self::NotFound => SessionResourceErrorKind::NotFound,
            Self::Unsupported => SessionResourceErrorKind::Unsupported,
            Self::ReadOnly => SessionResourceErrorKind::ReadOnlyStore,
            Self::Uncertain => SessionResourceErrorKind::PersistenceUncertain { thread_id: None },
            Self::Internal => SessionResourceErrorKind::Internal {
                detail: "fixture".to_owned(),
            },
        };
        SessionResourceError::new(kind)
    }
}

#[derive(Default)]
struct FakeState {
    sessions: BTreeMap<String, PortSessionRow>,
    items: BTreeMap<String, BTreeMap<u64, PortItemRow>>,
    inputs: HashMap<(String, String), PortInputRow>,
    bindings: HashMap<String, PortBindingRow>,
}

/// Port fake: stores the exact rows the adapter sends and can fail on demand.
struct FakePort {
    state: Mutex<FakeState>,
    fault: Mutex<Option<Fault>>,
    calls: AtomicUsize,
}

impl FakePort {
    fn new() -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            fault: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }
    }

    fn with_fault(fault: Fault) -> Self {
        let port = Self::new();
        *port.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(fault);
        port
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self) -> SessionResourceResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match *self.fault.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(fault) => Err(fault.error()),
            None => Ok(()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn stored_session(&self, logical_id: &str) -> PortSessionRow {
        self.lock()
            .sessions
            .get(logical_id)
            .cloned()
            .expect("session row written to the port")
    }

    fn insert_raw_session(&self, row: PortSessionRow) {
        self.lock().sessions.insert(row.logical_id.clone(), row);
    }
}

#[async_trait]
impl SwarmStorePort for FakePort {
    async fn swarm_upsert_session(&self, row: PortSessionRow) -> SessionResourceResult<()> {
        self.enter()?;
        let mut state = self.lock();
        if let Some(existing) = state.sessions.get(&row.logical_id) {
            if existing.root_session_id != row.root_session_id || existing.cwd != row.cwd {
                return Err(SessionResourceError::conflict("identity"));
            }
        }
        state.sessions.insert(row.logical_id.clone(), row);
        Ok(())
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> SessionResourceResult<Option<PortSessionRow>> {
        self.enter()?;
        Ok(self.lock().sessions.get(logical_id).cloned())
    }

    async fn swarm_update_session(&self, update: PortSessionUpdate) -> SessionResourceResult<()> {
        self.enter()?;
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(&update.logical_id)
            .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?;
        session.state = update.state;
        session.last_item_seq = update.last_item_seq;
        session.last_activity_at = update.last_activity_at;
        Ok(())
    }

    async fn swarm_append_item(&self, row: PortItemRow) -> SessionResourceResult<()> {
        self.enter()?;
        let mut state = self.lock();
        let items = state.items.entry(row.logical_id.clone()).or_default();
        if items.contains_key(&row.seq) {
            return Err(SessionResourceError::conflict("seq"));
        }
        items.insert(row.seq, row);
        Ok(())
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> SessionResourceResult<Vec<PortItemRow>> {
        self.enter()?;
        let state = self.lock();
        Ok(state.items.get(logical_id).map_or_else(Vec::new, |items| {
            items
                .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                .take(limit as usize)
                .map(|(_, row)| row.clone())
                .collect()
        }))
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> SessionResourceResult<Vec<PortItemRow>> {
        self.enter()?;
        let state = self.lock();
        Ok(state.items.get(logical_id).map_or_else(Vec::new, |items| {
            items
                .range(..before.unwrap_or(u64::MAX))
                .rev()
                .take(limit as usize)
                .map(|(_, row)| row.clone())
                .collect()
        }))
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> SessionResourceResult<u64> {
        self.enter()?;
        let state = self.lock();
        Ok(state
            .items
            .get(logical_id)
            .and_then(BTreeMap::last_key_value)
            .map_or(0, |(seq, _)| *seq))
    }

    async fn swarm_put_input(&self, row: PortInputRow) -> SessionResourceResult<PortInputPut> {
        self.enter()?;
        let mut state = self.lock();
        let key = (row.logical_id.clone(), row.input_id.clone());
        if let Some(existing) = state.inputs.get(&key) {
            return Ok(PortInputPut::Existing(existing.clone()));
        }
        state.inputs.insert(key, row);
        Ok(PortInputPut::Inserted)
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> SessionResourceResult<Option<PortInputRow>> {
        self.enter()?;
        Ok(self
            .lock()
            .inputs
            .get(&(logical_id.to_owned(), input_id.to_owned()))
            .cloned())
    }

    async fn swarm_put_binding(&self, row: PortBindingRow) -> SessionResourceResult<()> {
        self.enter()?;
        self.lock().bindings.insert(row.logical_env_id.clone(), row);
        Ok(())
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> SessionResourceResult<Option<PortBindingRow>> {
        self.enter()?;
        Ok(self.lock().bindings.get(logical_env_id).cloned())
    }
}

// ── fixtures ────────────────────────────────────────────────────────────────

fn environment(logical_id: &str) -> EnvironmentRef {
    EnvironmentRef {
        kind: EnvironmentKind::None,
        id: Some(logical_id.to_owned()),
    }
}

fn session_row(logical_id: &str) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: logical_id.to_owned(),
        root_session_id: "0192f0c4-0000-7000-8000-000000000001".to_owned(),
        actor_key: format!("agent/{logical_id}"),
        cwd: format!("/tmp/{logical_id}"),
        state: SessionState::Idle,
        model: Some("model-a".to_owned()),
        instructions: Some("be brief".to_owned()),
        mcp_tools: vec![McpToolSpec {
            headers: vec![McpHeader {
                name: "Authorization".to_owned(),
                value: "Bearer caller".to_owned(),
            }],
            require_approval: Some(json!("never")),
            allowed_tools: Some(json!({ "tool_names": ["read"], "read_only": true })),
            extra: Map::from_iter([("defer_loading".to_owned(), json!(true))]),
            ..McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp")
        }],
        environment: environment(logical_id),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_item_seq: 0,
    }
}

fn item_row(logical_id: &str, seq: u64) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: logical_id.to_owned(),
        seq,
        item_id: format!("itim_{logical_id}_{seq}"),
        kind: ItemKind::ToolResult,
        source: ItemSource {
            agent_id: "child:sub".to_owned(),
            task_id: Some("task-1".to_owned()),
        },
        payload: json!({ "text": format!("item {seq}") }),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        truncated: seq == 2,
    }
}

fn input_row(logical_id: &str, input_id: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: logical_id.to_owned(),
        input_id: input_id.to_owned(),
        fingerprint: "sha256:abc".to_owned(),
        state: InputState::HandedOff,
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
    }
}

fn binding_row(logical_env_id: &str) -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: logical_env_id.to_owned(),
        logical_id: "agent-1".to_owned(),
        sandbox_id: "sbx-1".to_owned(),
        workspace_path: "/workspace".to_owned(),
        sandbox_root: "/sandbox".to_owned(),
        settings_profile: Some("default".to_owned()),
        token_file: Some("/run/token".to_owned()),
        last_run_id: Some("run-1".to_owned()),
        last_used_at: "2026-01-01T00:00:00.000000Z".to_owned(),
    }
}

fn store(port: Arc<FakePort>) -> RemoteSwarmStore {
    RemoteSwarmStore::new(port)
}

// ── mapping and CRUD ────────────────────────────────────────────────────────

#[tokio::test]
async fn session_round_trip_preserves_every_field() {
    let port = Arc::new(FakePort::new());
    let store = store(Arc::clone(&port));
    let row = session_row("agent-1");
    store
        .swarm_upsert_session(row.clone())
        .await
        .expect("upsert");

    // The port must see the enum as its stable string and the environment as
    // canonical JSON, not as a Debug rendering or a dropped field.
    let stored = port.stored_session("agent-1");
    assert_eq!(stored.state, "idle");
    assert_eq!(
        stored.environment_json,
        serde_json::to_string(&row.environment).expect("environment json")
    );
    assert_eq!(stored.instructions.as_deref(), Some("be brief"));
    assert_eq!(
        stored.mcp_json.as_deref(),
        Some(
            r#"[{"type":"mcp","server_label":"deepwiki","server_url":"https://mcp.example.test/mcp","headers":[{"name":"Authorization","value":"Bearer caller"}],"require_approval":"never","allowed_tools":{"tool_names":["read"],"read_only":true},"defer_loading":true}]"#
        ),
        "MCP 声明必须以 canonical JSON 逐字段落库"
    );
    assert_eq!(stored.model.as_deref(), Some("model-a"));
    assert_eq!(stored.actor_key, "agent/agent-1");
    assert_eq!(stored.last_item_seq, 0);

    let read = store
        .swarm_get_session("agent-1")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(read, row);
}

#[tokio::test]
async fn missing_session_reads_as_none_not_an_error() {
    let port = Arc::new(FakePort::new());
    let store = store(port);
    assert_eq!(
        store.swarm_get_session("agent-none").await.expect("get"),
        None
    );
}

#[tokio::test]
async fn session_update_maps_state_and_sequence() {
    let port = Arc::new(FakePort::new());
    let store = store(Arc::clone(&port));
    store
        .swarm_upsert_session(session_row("agent-1"))
        .await
        .expect("upsert");

    store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: "agent-1".to_owned(),
            state: SessionState::Running,
            last_item_seq: 7,
            last_activity_at: "2026-01-02T00:00:00.000000Z".to_owned(),
        })
        .await
        .expect("update");

    let stored = port.stored_session("agent-1");
    assert_eq!(stored.state, "running");
    assert_eq!(stored.last_item_seq, 7);
    assert_eq!(stored.last_activity_at, "2026-01-02T00:00:00.000000Z");
}

#[tokio::test]
async fn item_pages_keep_order_payload_and_truncation() {
    let port = Arc::new(FakePort::new());
    let store = store(Arc::clone(&port));
    for seq in 1..=3 {
        store
            .swarm_append_item(item_row("agent-1", seq))
            .await
            .expect("append");
    }

    let stored = port
        .lock()
        .items
        .get("agent-1")
        .and_then(|items| items.get(&2))
        .cloned()
        .expect("port item 2");
    assert_eq!(stored.kind, "tool_result");
    assert_eq!(
        stored.source_json,
        serde_json::to_string(&item_row("agent-1", 2).source).expect("source json")
    );
    assert!(stored.truncated);

    let after = store
        .swarm_items_after("agent-1", 1, 10)
        .await
        .expect("after");
    assert_eq!(
        after.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(after[0], item_row("agent-1", 2));
    assert_eq!(after[1].payload, json!({ "text": "item 3" }));

    let before = store
        .swarm_items_before("agent-1", None, 2)
        .await
        .expect("before");
    assert_eq!(
        before.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![3, 2]
    );
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("last seq"), 3);
}

#[tokio::test]
async fn input_put_is_idempotent_and_returns_the_existing_receipt() {
    let port = Arc::new(FakePort::new());
    let store = store(port);
    let row = input_row("agent-1", "in_1");

    assert_eq!(
        store.swarm_put_input(row.clone()).await.expect("put"),
        InputPut::Inserted
    );
    assert_eq!(
        store.swarm_put_input(row.clone()).await.expect("replay"),
        InputPut::Existing(row.clone())
    );
    assert_eq!(
        store.swarm_get_input("agent-1", "in_1").await.expect("get"),
        Some(row)
    );
}

#[tokio::test]
async fn binding_round_trip_preserves_optional_fields() {
    let port = Arc::new(FakePort::new());
    let store = store(port);
    let row = binding_row("env-1");
    store.swarm_put_binding(row.clone()).await.expect("put");
    assert_eq!(
        store.swarm_get_binding("env-1").await.expect("get"),
        Some(row.clone())
    );
    assert_eq!(
        store.swarm_get_binding("env-none").await.expect("get"),
        None
    );
}

// ── failure classes ─────────────────────────────────────────────────────────

#[tokio::test]
async fn conflict_is_reported_once_and_never_retried() {
    let port = Arc::new(FakePort::with_fault(Fault::Conflict));
    let store = store(Arc::clone(&port));
    let error = store
        .swarm_upsert_session(session_row("agent-1"))
        .await
        .expect_err("conflict");
    assert!(matches!(error, StoreError::Conflict(_)), "{error:?}");
    assert_eq!(port.calls(), 1, "the adapter must not retry a conflict");
}

#[tokio::test]
async fn not_found_keeps_its_class() {
    let port = Arc::new(FakePort::with_fault(Fault::NotFound));
    let store = store(port);
    let error = store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: "agent-1".to_owned(),
            state: SessionState::Idle,
            last_item_seq: 0,
            last_activity_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        })
        .await
        .expect_err("not found");
    assert!(matches!(error, StoreError::NotFound(_)), "{error:?}");
}

#[tokio::test]
async fn unsupported_and_read_only_map_to_unsupported() {
    for fault in [Fault::Unsupported, Fault::ReadOnly] {
        let port = Arc::new(FakePort::with_fault(fault));
        let store = store(port);
        let error = store
            .swarm_get_session("agent-1")
            .await
            .expect_err("unsupported");
        assert!(
            matches!(error, StoreError::Unsupported(_)),
            "{fault:?} -> {error:?}"
        );
    }
}

#[tokio::test]
async fn uncertain_persistence_is_unavailable_and_says_the_outcome_is_unknown() {
    let port = Arc::new(FakePort::with_fault(Fault::Uncertain));
    let store = store(port);
    let error = store
        .swarm_append_item(item_row("agent-1", 1))
        .await
        .expect_err("uncertain");
    match error {
        StoreError::Unavailable(detail) => {
            assert!(detail.contains("unknown"), "{detail}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn internal_store_errors_are_unavailable() {
    let port = Arc::new(FakePort::with_fault(Fault::Internal));
    let store = store(port);
    let error = store.swarm_last_seq("agent-1").await.expect_err("internal");
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn a_corrupt_row_is_unavailable_instead_of_absent() {
    let port = Arc::new(FakePort::new());
    let mut raw = PortSessionRow {
        logical_id: "agent-1".to_owned(),
        root_session_id: "root".to_owned(),
        actor_key: "agent/agent-1".to_owned(),
        cwd: "/tmp/agent-1".to_owned(),
        state: "paused".to_owned(),
        model: None,
        instructions: None,
        mcp_json: None,
        environment_json: "{}".to_owned(),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_item_seq: 0,
    };
    port.insert_raw_session(raw.clone());

    let store = store(Arc::clone(&port));
    let error = store
        .swarm_get_session("agent-1")
        .await
        .expect_err("unknown state is not decodable");
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");

    raw.state = "idle".to_owned();
    raw.environment_json = "not json".to_owned();
    port.insert_raw_session(raw.clone());
    let error = store
        .swarm_get_session("agent-1")
        .await
        .expect_err("broken environment json is not decodable");
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");

    raw.environment_json = "{}".to_owned();
    raw.mcp_json = Some("not json".to_owned());
    port.insert_raw_session(raw);
    let error = store
        .swarm_get_session("agent-1")
        .await
        .expect_err("broken mcp declaration is not decodable");
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn a_legacy_session_without_mcp_json_reads_an_empty_declaration() {
    let port = Arc::new(FakePort::new());
    let mut raw = PortSessionRow {
        logical_id: "agent-1".to_owned(),
        root_session_id: "root".to_owned(),
        actor_key: "agent/agent-1".to_owned(),
        cwd: "/tmp/agent-1".to_owned(),
        state: "idle".to_owned(),
        model: None,
        instructions: None,
        mcp_json: None,
        environment_json: r#"{"type":"none","id":"agent-1"}"#.to_owned(),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_item_seq: 0,
    };
    port.insert_raw_session(raw.clone());

    let store = store(Arc::clone(&port));
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.mcp_tools, Vec::new(), "旧行 NULL 读回空列表");

    // 反向：空声明写回端口时也是 NULL，而不是 `[]` 文本。
    store.swarm_upsert_session(row).await.expect("upsert");
    assert_eq!(port.stored_session("agent-1").mcp_json, None);
}
