//! Schema and session contract tests for the PostgreSQL state store.
//!
//! The fixture (private schema per test, sample rows) lives in
//! [`super::test_fixture`]. These tests pin the fail-closed initializer and the
//! session/listing/receipt semantics; item, turn and binding contracts are in
//! [`super::contract_test`].

use std::collections::BTreeMap;

use crate::domain::{ItemOrder, SessionState, SessionsWindow};
use crate::store::{InputPut, StoreError, SwarmSessionPatch, SwarmStore};

use super::test_fixture::{input, session, TestSchema};
use super::{SwarmStateError, SwarmStateStore, SWARM_STATE_SCHEMA_VERSION};

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
async fn reopening_repairs_derived_objects_and_keeps_the_identity() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let identity = fixture.meta().await;
    store.close().await.expect("close");

    // A dropped index is a derived object: it is re-created. The identity row is
    // never rewritten.
    fixture.execute("DROP INDEX swarm_items_by_id").await;
    let store = fixture.store().await;
    assert_eq!(fixture.meta().await, identity, "identity is not rewritten");
    let tables = fixture.relations().await;
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
async fn a_missing_authoritative_table_fails_closed_instead_of_recreating_it() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");
    store
        .swarm_put_input(input("logical-1", "input-1", "fp"))
        .await
        .expect("receipt");
    store.close().await.expect("close");

    // Dropping an authoritative table is data loss. Re-creating it empty would
    // silently drop the idempotency receipt, so opening must fail.
    fixture.execute("DROP TABLE swarm_inputs").await;
    let error = SwarmStateStore::open(&fixture.config())
        .await
        .expect_err("a missing authoritative table must fail closed");
    let SwarmStateError::Schema(detail) = &error else {
        panic!("unexpected class: {error:?}");
    };
    assert!(detail.contains("swarm_inputs"), "{detail}");
    assert!(detail.contains("Restore"), "{detail}");
    assert!(
        !fixture
            .relations()
            .await
            .iter()
            .any(|(_, name)| name == "swarm_inputs"),
        "the open must not re-create the table"
    );

    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn a_missing_authoritative_column_fails_closed() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store.close().await.expect("close");
    fixture
        .execute("ALTER TABLE swarm_sessions DROP COLUMN metadata_json")
        .await;
    let error = SwarmStateStore::open(&fixture.config())
        .await
        .expect_err("a missing authoritative column must fail closed");
    assert!(
        matches!(&error, SwarmStateError::Schema(detail) if detail.contains("swarm_sessions.metadata_json")),
        "{error:?}"
    );
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
