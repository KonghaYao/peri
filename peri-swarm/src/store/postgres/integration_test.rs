//! Contract tests for the PostgreSQL state store against a real server.
//!
//! Every test creates a private schema and points the store at it through the
//! DSN's `options` parameter (`search_path`), so tests never see each other's
//! tables and the suite needs no `CREATEDB` privilege. Set
//! `PERI_TEST_POSTGRES_URL` to a database the role may create schemas in;
//! without it every test prints one skip line and returns.
//!
//! Run them with:
//!
//! ```text
//! PERI_TEST_POSTGRES_URL=postgresql://postgres@127.0.0.1:55432/postgres \
//!   scripts/cargo-rmcp-patched.sh test -p peri-swarm --lib -- \
//!   --ignored store::postgres::integration
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use url::Url;

use super::{SwarmStateError, SwarmStateStore, SwarmStateStoreConfig, SWARM_STATE_SCHEMA_VERSION};
use crate::config::PostgresLocator;
use crate::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemOrder, ItemSource, McpToolSpec,
    SessionState, SessionsWindow, TurnStatus, TurnsWindow,
};
use crate::store::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionPatch,
    SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};

const TEST_POSTGRES_ENV: &str = "PERI_TEST_POSTGRES_URL";

// ── fixture ─────────────────────────────────────────────────────────────────

/// A private schema on the test server.
struct TestSchema {
    schema: String,
    dsn: String,
}

impl TestSchema {
    async fn create() -> Option<Self> {
        let base = match std::env::var(TEST_POSTGRES_ENV) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!(
                    "skipping the PostgreSQL state store contract test: set {TEST_POSTGRES_ENV} \
                     to a test database"
                );
                return None;
            }
        };
        let schema = format!("swarm_it_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect PERI_TEST_POSTGRES_URL");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA {}",
            quote(&schema)
        )))
        .execute(&admin)
        .await
        .expect("create the private test schema");
        admin.close().await;

        let mut url = Url::parse(&base).expect("valid test DSN");
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let fixture = Self {
            schema,
            dsn: url.to_string(),
        };
        // The DSN rewrite must actually select the private schema.
        let probe = fixture.pool().await;
        let current: Option<String> = sqlx::query_scalar("SELECT current_schema()::text")
            .fetch_one(&probe)
            .await
            .expect("read current_schema");
        probe.close().await;
        assert_eq!(
            current.as_deref(),
            Some(fixture.schema.as_str()),
            "the test DSN must select {}",
            fixture.schema
        );
        Some(fixture)
    }

    fn config(&self) -> SwarmStateStoreConfig {
        SwarmStateStoreConfig {
            locator: PostgresLocator::parse(&self.dsn, TEST_POSTGRES_ENV)
                .expect("the test DSN is a PostgreSQL DSN"),
        }
    }

    async fn store(&self) -> SwarmStateStore {
        SwarmStateStore::open(&self.config())
            .await
            .expect("open the PostgreSQL state store")
    }

    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&self.dsn)
            .await
            .expect("probe pool")
    }

    async fn execute(&self, sql: &str) {
        let pool = self.pool().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&pool)
            .await
            .expect("fixture statement");
        pool.close().await;
    }

    /// `(relkind, relname)` of every relation in this schema.
    async fn relations(&self) -> Vec<(String, String)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT c.relkind::text, c.relname::text FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 ORDER BY c.relkind, c.relname",
        )
        .bind(self.schema.clone())
        .fetch_all(&pool)
        .await
        .expect("catalog relations");
        pool.close().await;
        rows.iter()
            .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
            .collect()
    }

    async fn meta(&self) -> Vec<(i64, String, String)> {
        let pool = self.pool().await;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT version, engine, contract FROM {}.swarm_meta",
            quote(&self.schema)
        )))
        .fetch_all(&pool)
        .await
        .expect("swarm_meta rows");
        pool.close().await;
        rows.iter()
            .map(|row| {
                (
                    row.get::<i64, _>(0),
                    row.get::<String, _>(1),
                    row.get::<String, _>(2),
                )
            })
            .collect()
    }

    async fn count(&self, table: &str) -> i64 {
        let pool = self.pool().await;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {}.{}",
            quote(&self.schema),
            quote(table)
        )))
        .fetch_one(&pool)
        .await
        .expect("row count");
        pool.close().await;
        count
    }

    async fn close(self) {
        let base = {
            let mut url = Url::parse(&self.dsn).expect("test DSN");
            url.set_query(None);
            url.to_string()
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("cleanup pool");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            quote(&self.schema)
        )))
        .execute(&admin)
        .await
        .expect("drop the private test schema");
        admin.close().await;
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn now() -> String {
    peri_time::now_utc_rfc3339()
}

fn session(logical_id: &str, cwd: &str) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: logical_id.to_owned(),
        root_session_id: format!("root-{logical_id}"),
        actor_key: format!("agent:{logical_id}"),
        cwd: cwd.to_owned(),
        state: SessionState::Idle,
        model: Some("stub-model".to_owned()),
        instructions: Some("be concise".to_owned()),
        mcp_tools: vec![McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp")],
        environment: EnvironmentRef::none(),
        metadata: BTreeMap::from([("tenant".to_owned(), "acme".to_owned())]),
        prompt_cache_options: None,
        created_at: now(),
        last_activity_at: now(),
        last_item_seq: 0,
    }
}

fn item(logical_id: &str, seq: u64, item_id: &str, text: &str) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: logical_id.to_owned(),
        seq,
        item_id: item_id.to_owned(),
        kind: ItemKind::Message,
        source: ItemSource::root(),
        payload: serde_json::json!({"text": text, "complete": true}),
        created_at: now(),
        truncated: false,
    }
}

fn input(logical_id: &str, input_id: &str, fingerprint: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: logical_id.to_owned(),
        input_id: input_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        state: InputState::Accepted,
        created_at: now(),
    }
}

fn binding(logical_env_id: &str, logical_id: &str) -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: logical_env_id.to_owned(),
        logical_id: logical_id.to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        workspace_path: "/workspace/cwd".to_owned(),
        sandbox_root: "/workspace".to_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-1".to_owned()),
        last_used_at: now(),
    }
}

fn turn(logical_id: &str, turn_id: &str, status: TurnStatus, created_at: i64) -> SwarmTurnRow {
    SwarmTurnRow {
        logical_id: logical_id.to_owned(),
        turn_id: turn_id.to_owned(),
        status,
        created_at,
        started_at: None,
        completed_at: None,
        error: None,
        usage: None,
        subagent_id: None,
    }
}

// ── schema ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn first_open_creates_the_owned_schema() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;

    let mut tables: Vec<String> = fixture
        .relations()
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "r" || kind == "p")
        .map(|(_, name)| name)
        .collect();
    tables.sort();
    assert_eq!(
        tables,
        vec![
            "swarm_bindings",
            "swarm_inputs",
            "swarm_items",
            "swarm_meta",
            "swarm_sessions",
            "swarm_turns",
        ],
        "the state schema owns exactly the six Swarm tables"
    );

    let indexes: Vec<String> = fixture
        .relations()
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "i" || kind == "I")
        .map(|(_, name)| name)
        .collect();
    for index in ["swarm_items_by_id", "swarm_turns_by_created"] {
        assert!(
            indexes.iter().any(|present| present == index),
            "missing index {index}: {indexes:?}"
        );
    }

    assert_eq!(
        fixture.meta().await,
        vec![(
            SWARM_STATE_SCHEMA_VERSION,
            "postgres".to_owned(),
            "peri.swarm.postgres/v1".to_owned()
        )],
        "swarm_meta holds exactly this build's identity"
    );

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn reopening_repairs_missing_objects_and_keeps_the_identity() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let identity = fixture.meta().await;
    store.close().await.expect("close");

    // A dropped index and table are repaired by the idempotent DDL; the
    // identity row is never rewritten.
    fixture
        .execute("DROP INDEX swarm_items_by_id; DROP TABLE swarm_inputs")
        .await;
    let store = fixture.store().await;
    assert_eq!(fixture.meta().await, identity, "identity is not rewritten");
    let tables = fixture.relations().await;
    assert!(
        tables.iter().any(|(_, name)| name == "swarm_inputs"),
        "the missing table is re-created: {tables:?}"
    );
    assert!(
        tables
            .iter()
            .any(|(kind, name)| (kind == "i" || kind == "I") && name == "swarm_items_by_id"),
        "the missing index is re-created: {tables:?}"
    );
    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn a_newer_schema_version_fails_closed() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store.close().await.expect("close");
    fixture.execute("UPDATE swarm_meta SET version = 2").await;

    let error = SwarmStateStore::open(&fixture.config())
        .await
        .expect_err("version 2 must not be accepted");
    assert!(
        matches!(
            error,
            SwarmStateError::SchemaVersion {
                found: 2,
                expected: 1
            }
        ),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("PERI_SWARM_STATE_STORE"),
        "the error must tell the operator what to do: {error}"
    );
    assert_eq!(
        fixture.meta().await,
        vec![(
            2,
            "postgres".to_owned(),
            "peri.swarm.postgres/v1".to_owned()
        )],
        "a rejected identity is left untouched"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn a_non_empty_schema_is_never_adopted() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    fixture
        .execute("CREATE TABLE foreign_table (id integer)")
        .await;
    let error = SwarmStateStore::open(&fixture.config())
        .await
        .expect_err("a non-empty schema must not be initialized");
    let SwarmStateError::Schema(detail) = &error else {
        panic!("unexpected class: {error:?}");
    };
    assert!(detail.contains("not empty"), "{detail}");
    assert!(detail.contains("foreign_table"), "{detail}");
    assert!(
        !fixture
            .relations()
            .await
            .iter()
            .any(|(_, name)| name == "swarm_sessions"),
        "nothing may be created in a foreign schema"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn the_peri_session_schema_is_never_adopted() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    fixture
        .execute("CREATE TABLE peri_store_meta (singleton bigint PRIMARY KEY)")
        .await;
    let error = SwarmStateStore::open(&fixture.config())
        .await
        .expect_err("the session schema must not be adopted");
    let SwarmStateError::Schema(detail) = &error else {
        panic!("unexpected class: {error:?}");
    };
    assert!(detail.contains("Peri session store"), "{detail}");
    assert!(detail.contains("different database"), "{detail}");
    fixture.close().await;
}

// ── sessions ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn session_rows_round_trip_and_guard_their_identity() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let row = session("logical-1", "/workspace/a");
    store
        .swarm_upsert_session(row.clone())
        .await
        .expect("insert");
    assert_eq!(
        store.swarm_get_session("logical-1").await.expect("read"),
        Some(row.clone()),
        "every column round-trips"
    );

    // Same identity, new state: the upsert replaces the mutable fields.
    let mut updated = row.clone();
    updated.state = SessionState::Running;
    updated.last_item_seq = 3;
    store
        .swarm_upsert_session(updated.clone())
        .await
        .expect("update in place");
    assert_eq!(
        store.swarm_get_session("logical-1").await.expect("read"),
        Some(updated.clone())
    );

    // Same logical id, different cwd: 0 rows, reported as a conflict.
    let mut moved = row.clone();
    moved.cwd = "/workspace/b".to_owned();
    let conflict = store
        .swarm_upsert_session(moved)
        .await
        .expect_err("identity conflict");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");

    // A second logical id claiming the root session id is a unique violation.
    let mut stolen = session("logical-2", "/workspace/a");
    stolen.root_session_id = row.root_session_id.clone();
    let conflict = store
        .swarm_upsert_session(stolen)
        .await
        .expect_err("duplicate root session id");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");

    // `sessions.update` replaces only the fields it carries.
    store
        .swarm_patch_session(SwarmSessionPatch {
            logical_id: "logical-1".to_owned(),
            model: Some("other-model".to_owned()),
            mcp_tools: Some(Vec::new()),
            metadata: Some(BTreeMap::new()),
            prompt_cache_options: Some(None),
            last_activity_at: None,
        })
        .await
        .expect("patch");
    let patched = store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(patched.model.as_deref(), Some("other-model"));
    assert!(patched.mcp_tools.is_empty());
    assert!(patched.metadata.is_empty());
    assert_eq!(patched.state, SessionState::Running);
    assert_eq!(patched.last_item_seq, 3);

    // A patch of an absent row is NotFound rather than a silent no-op.
    let missing = store
        .swarm_patch_session(SwarmSessionPatch {
            logical_id: "logical-absent".to_owned(),
            model: Some("other-model".to_owned()),
            mcp_tools: None,
            metadata: None,
            prompt_cache_options: None,
            last_activity_at: None,
        })
        .await
        .expect_err("absent row");
    assert!(matches!(missing, StoreError::NotFound(_)), "{missing:?}");

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn session_lists_page_by_id_and_filter_metadata() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    for index in 1..=3 {
        let mut row = session(&format!("logical-{index}"), "/workspace/a");
        row.metadata = BTreeMap::from([(
            "tenant".to_owned(),
            if index == 2 { "other" } else { "acme" }.to_owned(),
        )]);
        store.swarm_upsert_session(row).await.expect("seed");
    }

    let page = store
        .swarm_list_sessions(&SessionsWindow {
            order: ItemOrder::Asc,
            after: None,
            limit: 2,
            agent_id: None,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("ascending page");
    let ids: Vec<&str> = page.iter().map(|row| row.logical_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["logical-1", "logical-2", "logical-3"],
        "the store returns limit + 1 rows so the caller can derive has_more"
    );

    let next = store
        .swarm_list_sessions(&SessionsWindow {
            order: ItemOrder::Asc,
            after: Some("logical-2".to_owned()),
            limit: 2,
            agent_id: None,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("second page");
    assert_eq!(
        next.iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        vec!["logical-3"]
    );

    let filtered = store
        .swarm_list_sessions(&SessionsWindow {
            order: ItemOrder::Desc,
            after: None,
            limit: 10,
            agent_id: None,
            metadata: BTreeMap::from([("tenant".to_owned(), "acme".to_owned())]),
        })
        .await
        .expect("metadata filter");
    assert_eq!(
        filtered
            .iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        vec!["logical-3", "logical-1"],
        "only sessions whose metadata matches are listed"
    );

    let other_agent = store
        .swarm_list_sessions(&SessionsWindow {
            order: ItemOrder::Asc,
            after: None,
            limit: 10,
            agent_id: Some("sub".to_owned()),
            metadata: BTreeMap::new(),
        })
        .await
        .expect("agent filter");
    assert!(other_agent.is_empty(), "only the root agent exists");

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn inputs_are_insert_if_absent() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    let receipt = input("logical-1", "input-1", "fingerprint-1");
    assert_eq!(
        store
            .swarm_put_input(receipt.clone())
            .await
            .expect("insert"),
        InputPut::Inserted
    );
    match store
        .swarm_put_input(input("logical-1", "input-1", "fingerprint-2"))
        .await
        .expect("replay")
    {
        InputPut::Existing(existing) => {
            assert_eq!(
                existing.fingerprint, "fingerprint-1",
                "the first receipt wins and is returned to the caller"
            );
        }
        other => panic!("expected the existing receipt, got {other:?}"),
    }
    assert_eq!(
        store
            .swarm_get_input("logical-1", "input-1")
            .await
            .expect("read"),
        Some(receipt)
    );

    store.close().await.expect("close");
    fixture.close().await;
}

// ── items ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn items_page_by_item_and_advance_the_cursor() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    // Two streamed messages: item `a` has two frames, item `b` one.
    for (seq, item_id, text) in [(1, "a", "a0"), (2, "b", "b0"), (3, "a", "a1")] {
        store
            .swarm_append_item(item("logical-1", seq, item_id, text))
            .await
            .expect("append");
    }
    assert_eq!(
        store.swarm_last_seq("logical-1").await.expect("last seq"),
        3
    );

    let after: Vec<u64> = store
        .swarm_items_after("logical-1", 1, 10)
        .await
        .expect("after")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(after, vec![2, 3]);

    let before: Vec<u64> = store
        .swarm_items_before("logical-1", Some(3), 10)
        .await
        .expect("before")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(before, vec![2, 1]);
    let tail: Vec<u64> = store
        .swarm_items_before("logical-1", None, 2)
        .await
        .expect("tail")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(tail, vec![3, 2]);

    // A cursor is an item boundary: `after=1` selects whole items `b` and `a`
    // (the latter at its greatest seq), and reports the page cursor.
    let page = store
        .swarm_item_page("logical-1", ItemOrder::Asc, Some(1), 10)
        .await
        .expect("ascending page");
    assert_eq!(
        page.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        page.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "every row of a selected item belongs to the page, including seq 1 of item `a`"
    );

    // One item per page, newest item first: the page holds both frames of the
    // streamed message that started at seq 1, plus its terminal at seq 3.
    let first = store
        .swarm_item_page("logical-1", ItemOrder::Desc, None, 1)
        .await
        .expect("first item only");
    assert_eq!(
        first
            .items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a"],
        "an item is returned whole: both of its frames are in `rows`"
    );
    assert_eq!(
        first.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![3, 1]
    );
    assert!(first.has_more);
    assert_eq!(
        first.next_cursor,
        Some(3),
        "the cursor names the last item's greatest seq, not a row inside it"
    );

    // The next descending page (before the cursor) is the remaining item.
    let rest = store
        .swarm_item_page("logical-1", ItemOrder::Desc, first.next_cursor, 1)
        .await
        .expect("remaining item");
    assert_eq!(
        rest.items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
    assert_eq!(
        rest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert!(!rest.has_more);

    let descending = store
        .swarm_item_page("logical-1", ItemOrder::Desc, None, 10)
        .await
        .expect("descending page");
    assert_eq!(
        descending
            .items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );

    // `get_item` prefers the greatest seq of a repeated id.
    let greatest = store
        .swarm_get_item("logical-1", "a")
        .await
        .expect("get item")
        .expect("item");
    assert_eq!(greatest.seq, 3);

    // A repeated (logical_id, seq) is a conflict, not a renumbering.
    let conflict = store
        .swarm_append_item(item("logical-1", 3, "a", "duplicate"))
        .await
        .expect_err("duplicate seq");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn append_items_is_one_write_unit() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    // A conflicting row in the middle leaves none of the run persisted.
    let run = vec![
        item("logical-1", 1, "a", "a0"),
        item("logical-1", 2, "b", "b0"),
        item("logical-1", 2, "c", "c0"),
    ];
    let conflict = store
        .swarm_append_items(&run, None)
        .await
        .expect_err("duplicate seq inside the run");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");
    assert_eq!(
        fixture.count("swarm_items").await,
        0,
        "a failed run leaves no row behind"
    );

    // The progress update is part of the same unit: a missing session rolls the
    // items back too.
    let missing = store
        .swarm_append_items(
            &[item("logical-1", 1, "a", "a0")],
            Some(SwarmSessionUpdate {
                logical_id: "logical-absent".to_owned(),
                state: SessionState::Running,
                last_item_seq: 1,
                last_activity_at: now(),
            }),
        )
        .await
        .expect_err("the session does not exist");
    assert!(matches!(missing, StoreError::NotFound(_)), "{missing:?}");
    assert_eq!(fixture.count("swarm_items").await, 0);

    // A successful run commits items and the cursor together.
    store
        .swarm_append_items(
            &[item("logical-1", 1, "a", "a0")],
            Some(SwarmSessionUpdate {
                logical_id: "logical-1".to_owned(),
                state: SessionState::Running,
                last_item_seq: 1,
                last_activity_at: now(),
            }),
        )
        .await
        .expect("append run");
    let session_row = store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(session_row.state, SessionState::Running);
    assert_eq!(session_row.last_item_seq, 1);
    assert_eq!(fixture.count("swarm_items").await, 1);

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn concurrent_appends_leave_one_row_and_a_conflict() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = Arc::new(fixture.store().await);
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    let first = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .swarm_append_item(item("logical-1", 1, "a", "a0"))
                .await
        })
    };
    let second = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .swarm_append_item(item("logical-1", 1, "b", "b0"))
                .await
        })
    };
    let outcomes = [first.await.expect("join"), second.await.expect("join")];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "the primary key arbitrates the race: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Err(StoreError::Conflict(_)))),
        "the loser sees a conflict so it can reload MAX(seq): {outcomes:?}"
    );
    assert_eq!(fixture.count("swarm_items").await, 1);

    store.close().await.expect("close");
    fixture.close().await;
}

// ── turns, bindings, hosted sessions ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn turns_keep_created_at_and_close_open_rows() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    store
        .swarm_upsert_turn(turn("logical-1", "turn-1", TurnStatus::InProgress, 100))
        .await
        .expect("open turn");
    assert!(store
        .swarm_has_open_turn("logical-1")
        .await
        .expect("open probe"));

    let mut completed = turn("logical-1", "turn-1", TurnStatus::Completed, 999);
    completed.started_at = Some(100);
    completed.completed_at = Some(101);
    completed.error = Some(serde_json::json!({"code": "none"}));
    completed.usage = Some(serde_json::json!({"input_tokens": 3}));
    store
        .swarm_upsert_turn(completed.clone())
        .await
        .expect("terminal write");
    let stored = store
        .swarm_get_turn("logical-1", "turn-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(stored.status, TurnStatus::Completed);
    assert_eq!(
        stored.created_at, 100,
        "the terminal write keeps the opening created_at"
    );
    assert_eq!(stored.started_at, Some(100));
    assert_eq!(stored.completed_at, Some(101));
    assert_eq!(stored.error, completed.error);
    assert_eq!(stored.usage, completed.usage);
    assert!(!store
        .swarm_has_open_turn("logical-1")
        .await
        .expect("open probe"));

    let page = store
        .swarm_turn_page(
            "logical-1",
            &TurnsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: 10,
            },
        )
        .await
        .expect("turn page");
    assert_eq!(page.len(), 1);

    // Crash reconciliation closes every non-terminal row of the session.
    store
        .swarm_upsert_turn(turn("logical-1", "turn-2", TurnStatus::Waiting, 200))
        .await
        .expect("open second turn");
    let closed = store
        .swarm_close_open_turns(
            "logical-1",
            TurnStatus::Failed,
            300,
            Some(serde_json::json!({"code": "host_restarted"})),
        )
        .await
        .expect("close open turns");
    assert_eq!(closed, 1);
    let stored = store
        .swarm_get_turn("logical-1", "turn-2")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(stored.status, TurnStatus::Failed);
    assert_eq!(stored.completed_at, Some(300));
    assert_eq!(
        store
            .swarm_close_open_turns("logical-1", TurnStatus::Failed, 301, None)
            .await
            .expect("no open turns"),
        0
    );

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn delete_session_is_atomic_and_reassigns_bindings() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let shared = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-1".to_owned()),
    };
    let mut first = session("logical-1", "/workspace/a");
    first.environment = shared.clone();
    let mut second = session("logical-2", "/workspace/a");
    second.environment = shared;
    for row in [first, second] {
        store.swarm_upsert_session(row).await.expect("seed");
    }
    store
        .swarm_append_item(item("logical-1", 1, "a", "a0"))
        .await
        .expect("item");
    store
        .swarm_put_input(input("logical-1", "input-1", "fp"))
        .await
        .expect("input");
    store
        .swarm_upsert_turn(turn("logical-1", "turn-1", TurnStatus::Completed, 1))
        .await
        .expect("turn");
    store
        .swarm_put_binding(binding("logical-env-1", "logical-1"))
        .await
        .expect("binding");

    store
        .swarm_delete_session("logical-1")
        .await
        .expect("delete");

    for table in ["swarm_items", "swarm_inputs", "swarm_turns"] {
        assert_eq!(
            fixture.count(table).await,
            0,
            "{table} rows of the deleted session are gone"
        );
    }
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("the shared environment still exists")
            .logical_id,
        "logical-2",
        "the binding is handed to the remaining user of the environment"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_none());

    // Deleting the last user removes the binding as well.
    store
        .swarm_delete_session("logical-2")
        .await
        .expect("delete");
    assert!(store
        .swarm_get_binding("logical-env-1")
        .await
        .expect("binding")
        .is_none());

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn hosted_session_requires_a_matching_binding() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let mut row = session("logical-1", "/workspace/a");
    row.environment = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-1".to_owned()),
    }
    .normalized();
    let existing = binding("logical-env-1", "logical-0");
    store
        .swarm_put_binding(existing.clone())
        .await
        .expect("binding");

    // The sandbox no longer matches: the session is not created and the binding
    // keeps pointing at its previous session.
    let mut mismatched = existing.clone();
    mismatched.sandbox_id = "sandbox-2".to_owned();
    mismatched.last_used_at = "2099-01-01T00:00:00Z".to_owned();
    let error = store
        .swarm_create_hosted_session(row.clone(), mismatched, true)
        .await
        .expect_err("the binding does not match");
    assert!(matches!(error, StoreError::NotFound(_)), "{error:?}");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding"),
        Some(existing.clone()),
        "a rejected attach changes nothing"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_none());

    // A matching attach writes the session and moves the binding. The caller
    // supplies the binding row of the **new** session.
    let mut attached = existing.clone();
    attached.logical_id = "logical-1".to_owned();
    attached.workspace_path = "/workspace/cwd".to_owned();
    store
        .swarm_create_hosted_session(row.clone(), attached.clone(), true)
        .await
        .expect("attach");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("binding")
            .logical_id,
        "logical-1"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_some());

    // A new environment upserts the binding and inserts the session together.
    let mut fresh = session("logical-2", "/workspace/a");
    fresh.environment = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-2".to_owned()),
    }
    .normalized();
    store
        .swarm_create_hosted_session(fresh, binding("logical-env-2", "logical-2"), false)
        .await
        .expect("new hosted session");
    assert!(store
        .swarm_get_session("logical-2")
        .await
        .expect("read")
        .is_some());

    // An identity conflict rolls the binding update back.
    let mut conflicting = row;
    conflicting.cwd = "/workspace/b".to_owned();
    let conflict = store
        .swarm_create_hosted_session(conflicting, attached, true)
        .await
        .expect_err("identity conflict");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("binding")
            .logical_id,
        "logical-1",
        "the binding update is part of the rolled-back transaction"
    );

    store.close().await.expect("close");
    fixture.close().await;
}
