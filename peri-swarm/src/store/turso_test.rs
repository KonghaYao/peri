//! Offline tests for the independent Swarm state store (S6c1a).
//!
//! There is no Loom in this environment, so these tests cover the parts that
//! can be proven without a server: the owned DDL/version contract, the
//! statement/parameter shape, the row encoding (including `mcp_json` and
//! repeated `item_id` rows) and the driver-error classification. The real
//! HTTP round trip against Loom is S6c2's acceptance.

use serde_json::json;
use turso_serverless::Error as SdkError;
use url::Url;

use super::{
    binding_params, classify, decode_binding, decode_input, decode_item, decode_session,
    decode_turn, input_params, item_params, migration_statements, require_supported_version,
    resolve_credential, seq_param, session_list_sql, session_params, turn_params, SwarmStateError,
    SwarmStateStoreConfig, CLOSE_OPEN_TURNS_SQL, CREATE_INDEX_SQL, CREATE_META_TABLE_SQL,
    CREATE_TABLE_SQL, DELETE_BINDINGS_SQL, DELETE_INPUTS_SQL, DELETE_ITEMS_SQL, DELETE_SESSION_SQL,
    DELETE_TURNS_SQL, INSERT_INPUT_SQL, INSERT_ITEM_SQL, META_TABLE_EXISTS_SQL, MIGRATION_COLUMNS,
    PATCH_SESSION_SQL, PIN_VERSION_SQL, SELECT_BINDING_SQL, SELECT_INPUT_SQL,
    SELECT_ITEMS_AFTER_SQL, SELECT_ITEMS_BEFORE_SQL, SELECT_ITEMS_TAIL_SQL,
    SELECT_ITEM_PAGE_ASC_SQL, SELECT_ITEM_PAGE_DESC_SQL, SELECT_ITEM_PAGE_TAIL_SQL,
    SELECT_LAST_SEQ_SQL, SELECT_SESSION_SQL, SELECT_TURNS_ASC_AFTER_SQL, SELECT_TURNS_ASC_SQL,
    SELECT_TURNS_DESC_AFTER_SQL, SELECT_TURNS_DESC_SQL, SELECT_TURN_SQL, SELECT_VERSION_SQL,
    SESSIONS_TABLE_EXISTS_SQL, SESSIONS_TABLE_INFO_SQL, SWARM_STATE_SCHEMA_VERSION,
    UPDATE_SESSION_SQL, UPSERT_BINDING_SQL, UPSERT_SESSION_SQL, UPSERT_TURN_SQL,
};
use crate::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemOrder, ItemSource, McpHeader,
    McpToolSpec, SessionState, TurnStatus,
};
use crate::store::{
    StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmTurnRow,
};
use turso_serverless::Value;

// ── fixtures ────────────────────────────────────────────────────────────────

fn sample_session() -> SwarmSessionRow {
    let mut tool = McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp");
    tool.headers = vec![McpHeader {
        name: "Authorization".to_owned(),
        value: "Bearer caller-supplied".to_owned(),
    }];
    tool.require_approval = Some(json!("never"));
    SwarmSessionRow {
        logical_id: "logical-1".to_owned(),
        root_session_id: "root-1".to_owned(),
        actor_key: "actor-1".to_owned(),
        cwd: "/tmp/peri-swarm".to_owned(),
        state: SessionState::Idle,
        model: Some("stub-model".to_owned()),
        instructions: Some("be concise".to_owned()),
        mcp_tools: vec![tool],
        environment: EnvironmentRef {
            kind: EnvironmentKind::None,
            id: Some("logical-1".to_owned()),
        },
        metadata: Default::default(),
        prompt_cache_options: None,
        created_at: "2026-11-20T00:00:00Z".to_owned(),
        last_activity_at: "2026-11-20T00:00:01Z".to_owned(),
        last_item_seq: 7,
    }
}

fn sample_item(seq: u64, kind: ItemKind, payload: serde_json::Value) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: "logical-1".to_owned(),
        seq,
        item_id: "item-stable".to_owned(),
        kind,
        source: ItemSource::root(),
        payload,
        created_at: "2026-11-20T00:00:02Z".to_owned(),
        truncated: false,
    }
}

fn sample_input() -> SwarmInputRow {
    SwarmInputRow {
        logical_id: "logical-1".to_owned(),
        input_id: "input-1".to_owned(),
        fingerprint: "sha256:abc".to_owned(),
        state: InputState::Accepted,
        created_at: "2026-11-20T00:00:03Z".to_owned(),
    }
}

fn sample_binding() -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: "env-1".to_owned(),
        logical_id: "logical-1".to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        workspace_path: "/tmp/peri-swarm".to_owned(),
        sandbox_root: "/tmp/peri-swarm/sandboxes".to_owned(),
        settings_profile: Some("default".to_owned()),
        token_file: Some("/tmp/peri-swarm/token".to_owned()),
        last_run_id: Some("run-1".to_owned()),
        last_used_at: "2026-11-20T00:00:04Z".to_owned(),
    }
}

fn placeholders(sql: &str) -> usize {
    sql.matches('?').count()
}

/// Collapse whitespace so column assertions do not depend on alignment.
fn normalized(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn all_schema_statements() -> Vec<&'static str> {
    let mut statements = vec![CREATE_META_TABLE_SQL];
    statements.extend(CREATE_TABLE_SQL);
    statements.extend(CREATE_INDEX_SQL);
    statements
}

// ── schema contract ─────────────────────────────────────────────────────────

#[test]
fn schema_version_is_the_first_owned_contract() {
    assert_eq!(SWARM_STATE_SCHEMA_VERSION, 1);
}

#[test]
fn schema_creates_only_idempotent_swarm_objects() {
    for sql in all_schema_statements() {
        assert!(
            sql.starts_with("CREATE TABLE IF NOT EXISTS")
                || sql.starts_with("CREATE INDEX IF NOT EXISTS"),
            "schema statements must be idempotent: {sql}"
        );
    }
    let joined = all_schema_statements().join("\n");
    for table in [
        "swarm_meta",
        "swarm_sessions",
        "swarm_items",
        "swarm_inputs",
        "swarm_bindings",
    ] {
        assert!(joined.contains(table), "missing table {table}");
    }
    assert!(joined.contains("swarm_items_by_id"));
    for peri_object in ["peri_store_meta", "threads", "messages", "session_bindings"] {
        assert!(
            !joined.contains(peri_object),
            "swarm state schema must not reference the Peri canonical object {peri_object}"
        );
    }
}

#[test]
fn session_schema_carries_mcp_json_and_identity_columns() {
    let sessions = normalized(CREATE_TABLE_SQL[0]);
    assert!(sessions.contains("logical_id TEXT PRIMARY KEY"));
    assert!(sessions.contains("root_session_id TEXT NOT NULL UNIQUE"));
    assert!(sessions.contains("mcp_json TEXT"));
    assert!(sessions.contains("environment_json TEXT NOT NULL"));
    assert!(sessions.contains("last_item_seq INTEGER NOT NULL DEFAULT 0"));
}

#[test]
fn item_schema_keys_on_logical_seq_and_allows_repeated_item_ids() {
    let items = normalized(CREATE_TABLE_SQL[1]);
    assert!(items.contains("PRIMARY KEY (logical_id, seq)"));
    assert!(
        !items.contains("UNIQUE"),
        "item_id must stay non-unique (delta and complete rows share one id)"
    );
    let index = normalized(CREATE_INDEX_SQL[0]);
    assert!(index.contains("ON swarm_items (logical_id, item_id)"));
}

#[test]
fn version_is_pinned_once_and_read_from_the_owned_meta_table() {
    assert!(PIN_VERSION_SQL.contains("WHERE NOT EXISTS (SELECT 1 FROM swarm_meta)"));
    assert_eq!(placeholders(PIN_VERSION_SQL), 1);
    assert!(SELECT_VERSION_SQL.contains("FROM swarm_meta"));
    assert!(META_TABLE_EXISTS_SQL.contains("sqlite_master"));
    assert!(META_TABLE_EXISTS_SQL.contains("'swarm_meta'"));
    assert!(SESSIONS_TABLE_EXISTS_SQL.contains("'swarm_sessions'"));
}

#[test]
fn only_the_current_version_is_supported() {
    assert!(require_supported_version(SWARM_STATE_SCHEMA_VERSION).is_ok());
    match require_supported_version(SWARM_STATE_SCHEMA_VERSION + 1) {
        Err(SwarmStateError::SchemaVersion { found, expected }) => {
            assert_eq!(found, SWARM_STATE_SCHEMA_VERSION + 1);
            assert_eq!(expected, SWARM_STATE_SCHEMA_VERSION);
        }
        other => panic!("expected SchemaVersion, got {other:?}"),
    }
    assert!(require_supported_version(0).is_err());
}

// ── statement shape ─────────────────────────────────────────────────────────

#[test]
fn write_statements_bind_exactly_their_placeholders() {
    assert_eq!(
        placeholders(UPSERT_SESSION_SQL),
        session_params(&sample_session())
            .expect("session params")
            .len()
    );
    assert_eq!(placeholders(UPDATE_SESSION_SQL), 4);
    assert_eq!(
        placeholders(INSERT_ITEM_SQL),
        item_params(&sample_item(1, ItemKind::Message, json!({"text": "hi"})))
            .expect("item params")
            .len()
    );
    assert_eq!(
        placeholders(INSERT_INPUT_SQL),
        input_params(&sample_input()).len()
    );
    assert_eq!(
        placeholders(UPSERT_BINDING_SQL),
        binding_params(&sample_binding()).len()
    );
}

#[test]
fn read_statements_bind_one_cursor_shaped_parameter_set() {
    assert_eq!(placeholders(SELECT_SESSION_SQL), 1);
    assert_eq!(placeholders(SELECT_ITEMS_AFTER_SQL), 3);
    assert_eq!(placeholders(SELECT_ITEMS_BEFORE_SQL), 3);
    assert_eq!(placeholders(SELECT_ITEMS_TAIL_SQL), 2);
    assert_eq!(placeholders(SELECT_ITEM_PAGE_ASC_SQL), 3);
    assert_eq!(placeholders(SELECT_ITEM_PAGE_DESC_SQL), 3);
    assert_eq!(placeholders(SELECT_ITEM_PAGE_TAIL_SQL), 2);
    assert_eq!(placeholders(SELECT_LAST_SEQ_SQL), 1);
    assert_eq!(placeholders(SELECT_INPUT_SQL), 2);
    assert_eq!(placeholders(SELECT_BINDING_SQL), 1);
}

#[test]
fn item_page_statements_group_by_item_not_by_row() {
    for (sql, bound) in [
        (SELECT_ITEM_PAGE_ASC_SQL, "MAX(seq) > ?2"),
        (SELECT_ITEM_PAGE_DESC_SQL, "MAX(seq) < ?2"),
    ] {
        let normalized = normalized(sql);
        assert!(
            normalized.contains("GROUP BY logical_id, item_id"),
            "the page unit is the item: {normalized}"
        );
        assert!(
            normalized.contains(bound),
            "the cursor bounds the item's greatest seq: {normalized}"
        );
        assert!(
            normalized.contains("ORDER BY last_seq"),
            "page membership follows the item boundary: {normalized}"
        );
        assert!(
            normalized
                .contains("JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id"),
            "every row of a selected item is returned: {normalized}"
        );
    }
    let tail = normalized(SELECT_ITEM_PAGE_TAIL_SQL);
    assert!(tail.contains("GROUP BY logical_id, item_id"), "{tail}");
    assert!(
        !tail.contains("HAVING"),
        "no cursor means every item: {tail}"
    );
}

// ── row encoding ────────────────────────────────────────────────────────────

#[test]
fn session_roundtrip_preserves_every_field_and_the_mcp_declaration() {
    let row = sample_session();
    let params = session_params(&row).expect("params");
    let stored_mcp = match &params[7] {
        Value::Text(text) => text.clone(),
        other => panic!("mcp_json must be text, got {other:?}"),
    };
    assert!(stored_mcp.contains("\"server_label\":\"deepwiki\""));
    assert!(stored_mcp.contains("\"Authorization\""));
    assert_eq!(decode_session(&params).expect("decoded"), row);
}

#[test]
fn creating_state_roundtrips_as_the_durable_reservation() {
    let mut row = sample_session();
    row.state = SessionState::Creating;
    let params = session_params(&row).expect("params");
    // The wire value is what an interrupted create is read back as on the next
    // process; a decoder that refused it would strand the reserved root id.
    assert_eq!(params[4], Value::Text("creating".to_owned()));
    assert_eq!(decode_session(&params).expect("decoded"), row);
}

#[test]
fn session_without_mcp_tools_stores_null_and_reads_back_empty() {
    let mut row = sample_session();
    row.mcp_tools.clear();
    row.model = None;
    row.instructions = None;
    let params = session_params(&row).expect("params");
    assert_eq!(params[5], Value::Null);
    assert_eq!(params[6], Value::Null);
    assert_eq!(params[7], Value::Null);
    // NULL is also what rows written before S8a1 carry; both read as no tools.
    let decoded = decode_session(&params).expect("decoded");
    assert!(decoded.mcp_tools.is_empty());
    assert_eq!(decoded.model, None);
}

#[test]
fn two_rows_may_share_one_item_id_at_different_seqs() {
    let delta = sample_item(3, ItemKind::Message, json!({"delta": "hel"}));
    let complete = sample_item(4, ItemKind::Message, json!({"text": "hello"}));
    assert_eq!(delta.item_id, complete.item_id);

    let delta_params = item_params(&delta).expect("delta");
    let complete_params = item_params(&complete).expect("complete");
    assert_eq!(decode_item(&delta_params).expect("delta"), delta);
    assert_eq!(decode_item(&complete_params).expect("complete"), complete);
    assert_eq!(delta_params[2], complete_params[2]);
    assert_ne!(delta_params[1], complete_params[1]);
}

#[test]
fn item_roundtrip_keeps_kind_source_payload_and_truncation() {
    let mut row = sample_item(9, ItemKind::ToolResult, json!({"output": "ok"}));
    row.truncated = true;
    row.source = ItemSource {
        agent_id: "child".to_owned(),
        task_id: Some("task-1".to_owned()),
    };
    let params = item_params(&row).expect("params");
    assert_eq!(params[7], Value::Integer(1));
    assert_eq!(decode_item(&params).expect("decoded"), row);
}

#[test]
fn input_roundtrip_preserves_the_receipt_state() {
    let row = sample_input();
    let params = input_params(&row);
    assert_eq!(decode_input(&params).expect("decoded"), row);
}

#[test]
fn binding_roundtrip_covers_present_and_absent_optional_fields() {
    let full = sample_binding();
    assert_eq!(
        decode_binding(&binding_params(&full)).expect("decoded"),
        full
    );

    let mut sparse = sample_binding();
    sparse.settings_profile = None;
    sparse.token_file = None;
    sparse.last_run_id = None;
    let params = binding_params(&sparse);
    assert_eq!(params[5], Value::Null);
    assert_eq!(decode_binding(&params).expect("decoded"), sparse);
}

// ── unreadable rows and parameters ──────────────────────────────────────────

#[test]
fn unknown_enum_text_is_unreadable_not_absent() {
    let mut params = session_params(&sample_session()).expect("params");
    params[4] = Value::Text("bogus".to_owned());
    assert_unavailable(decode_session(&params).expect_err("unknown state"));

    let mut params = item_params(&sample_item(1, ItemKind::Message, json!({}))).expect("params");
    params[3] = Value::Text("bogus".to_owned());
    assert_unavailable(decode_item(&params).expect_err("unknown kind"));

    let mut params = input_params(&sample_input());
    params[3] = Value::Text("bogus".to_owned());
    assert_unavailable(decode_input(&params).expect_err("unknown input state"));
}

#[test]
fn wrong_columns_are_unreadable() {
    let params = session_params(&sample_session()).expect("params");
    assert_unavailable(decode_session(&params[..11]).expect_err("missing column"));

    let mut params = item_params(&sample_item(1, ItemKind::Message, json!({}))).expect("params");
    params[1] = Value::Integer(-1);
    assert_unavailable(decode_item(&params).expect_err("negative seq"));
}

#[test]
fn seq_beyond_the_signed_range_is_reported_not_wrapped() {
    let error = seq_param(u64::MAX, "seq").expect_err("overflow");
    assert_unavailable(error);
}

// ── error classification ────────────────────────────────────────────────────

#[test]
fn constraint_errors_are_conflicts() {
    let error = classify(
        "upsert_session",
        SdkError::Constraint("UNIQUE constraint failed".to_owned()),
    );
    match error {
        StoreError::Conflict(detail) => assert!(detail.contains("UNIQUE constraint failed")),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn a_failed_batch_statement_keeps_the_conflict_class() {
    let error = classify(
        "schema",
        SdkError::BatchStatementFailed {
            index: 2,
            error: Box::new(SdkError::Constraint("duplicate".to_owned())),
            results: Vec::new(),
        },
    );
    match error {
        StoreError::Conflict(detail) => {
            assert!(detail.contains("batch statement 2"), "{detail}");
            assert!(detail.contains("duplicate"), "{detail}");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn readonly_is_unsupported() {
    match classify("upsert_session", SdkError::Readonly("readonly".to_owned())) {
        StoreError::Unsupported(detail) => assert!(detail.contains("read only")),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn transport_and_storage_failures_are_unavailable() {
    for error in [
        SdkError::Http("connection refused".to_owned()),
        SdkError::Busy("locked".to_owned()),
        SdkError::Corrupt("bad page".to_owned()),
        SdkError::Error("boom".to_owned()),
    ] {
        assert_unavailable(classify("get_session", error));
    }
}

#[test]
fn a_failed_rollback_is_uncertain_not_successful() {
    let error = classify(
        "append_item",
        SdkError::BatchRollbackFailed {
            error: Box::new(SdkError::Constraint("conflict".to_owned())),
            rollback_error: Box::new(SdkError::Http("gone".to_owned())),
        },
    );
    match error {
        StoreError::Unavailable(detail) => assert!(detail.contains("unknown"), "{detail}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

// ── opening configuration ───────────────────────────────────────────────────

#[test]
fn missing_credential_names_the_variable_without_a_value() {
    let error = resolve_credential(|_| None, "PERI_TEST_LOOM_TOKEN").expect_err("missing");
    match &error {
        SwarmStateError::Credential { variable, reason } => {
            assert_eq!(variable, "PERI_TEST_LOOM_TOKEN");
            assert_eq!(*reason, "not set");
        }
        other => panic!("expected Credential, got {other:?}"),
    }
    assert!(error.to_string().contains("PERI_TEST_LOOM_TOKEN"));
}

#[test]
fn empty_credential_is_rejected_by_name() {
    let error =
        resolve_credential(|_| Some("   ".to_owned()), "PERI_TEST_LOOM_TOKEN").expect_err("empty");
    match &error {
        SwarmStateError::Credential { reason, .. } => assert_eq!(*reason, "set but empty"),
        other => panic!("expected Credential, got {other:?}"),
    }
}

#[test]
fn config_debug_renders_the_locator_but_never_a_token() {
    let config = SwarmStateStoreConfig {
        locator: Url::parse("http://127.0.0.1:9497/db/local_default/swarm/main/").expect("locator"),
        credential_env: "PERI_TEST_LOOM_TOKEN".to_owned(),
    };
    let rendered = format!("{config:?}");
    assert!(rendered.contains("9497"));
    assert!(rendered.contains("PERI_TEST_LOOM_TOKEN"));
}

fn assert_unavailable(error: StoreError) {
    match error {
        StoreError::Unavailable(detail) => assert!(!detail.is_empty(), "empty detail"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

// ── post-v1 additive migration and official session resources ───────────────

fn legacy_session_columns() -> Vec<String> {
    [
        "logical_id",
        "root_session_id",
        "actor_key",
        "cwd",
        "state",
        "model",
        "instructions",
        "mcp_json",
        "environment_json",
        "created_at",
        "last_activity_at",
        "last_item_seq",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn migration_adds_only_the_missing_session_columns() {
    let legacy = legacy_session_columns();
    let plan = migration_statements(&legacy);
    assert_eq!(
        plan,
        MIGRATION_COLUMNS
            .iter()
            .map(|(_, sql)| *sql)
            .collect::<Vec<_>>(),
        "a pre-v1 table gains both nullable columns"
    );
    assert!(plan.iter().all(|sql| sql.starts_with("ALTER TABLE")));
    assert!(plan[0].contains("metadata_json"));
    assert!(plan[1].contains("prompt_cache_options_json"));

    // After the first migration the table carries both columns: reopening
    // must not issue another ALTER (SQLite rejects a duplicate column).
    let migrated: Vec<String> = legacy
        .into_iter()
        .chain([
            "metadata_json".to_owned(),
            "prompt_cache_options_json".to_owned(),
        ])
        .collect();
    assert!(migration_statements(&migrated).is_empty());

    // A fresh database takes the CREATE TABLE path; empty means "table absent",
    // so no ALTER is attempted before the table exists.
    assert!(migration_statements(&[]).is_empty());

    assert!(SESSIONS_TABLE_INFO_SQL.contains("PRAGMA table_info(swarm_sessions)"));
}

#[test]
fn session_schema_carries_metadata_and_prompt_cache_columns() {
    let sessions = normalized(CREATE_TABLE_SQL[0]);
    assert!(sessions.contains("metadata_json TEXT"));
    assert!(sessions.contains("prompt_cache_options_json TEXT"));
    // Both are nullable: an older row reads back as "no metadata"/"no TTL".
    assert!(!sessions.contains("metadata_json TEXT NOT NULL"));
    assert!(!sessions.contains("prompt_cache_options_json TEXT NOT NULL"));
}

#[test]
fn turns_schema_is_keyed_for_exclusive_paging() {
    let turns = normalized(CREATE_TABLE_SQL[4]);
    assert!(turns.starts_with("CREATE TABLE IF NOT EXISTS swarm_turns"));
    assert!(turns.contains("PRIMARY KEY (logical_id, turn_id)"));
    assert!(turns.contains("status TEXT NOT NULL"));
    assert!(turns.contains("created_at INTEGER NOT NULL"));
    assert!(turns.contains("started_at INTEGER"));
    assert!(turns.contains("completed_at INTEGER"));
    assert!(turns.contains("error_json TEXT"));
    assert!(turns.contains("usage_json TEXT"));
    assert!(turns.contains("subagent_id TEXT"));

    let index = normalized(CREATE_INDEX_SQL[1]);
    assert!(index.contains("ON swarm_turns (logical_id, created_at, turn_id)"));
}

#[test]
fn session_list_sql_adds_one_json_each_predicate_per_metadata_pair() {
    let plain = session_list_sql(ItemOrder::Desc, 0);
    let plain = normalized(&plain);
    assert!(plain.contains("ORDER BY s.logical_id DESC LIMIT ?2"));
    assert!(!plain.contains("json_each"));

    let filtered = session_list_sql(ItemOrder::Asc, 2);
    let filtered = normalized(&filtered);
    assert_eq!(filtered.matches("json_each(s.metadata_json)").count(), 2);
    assert!(filtered.contains("m.key = ?3 AND m.value = ?4"));
    assert!(filtered.contains("m.key = ?5 AND m.value = ?6"));
    assert!(
        filtered.find("json_each").expect("predicate") < filtered.find("ORDER BY").expect("order"),
        "filters precede the order/limit suffix: {filtered}"
    );
}

#[test]
fn turn_statements_bind_their_placeholders_and_keyset_columns() {
    let sample = SwarmTurnRow {
        logical_id: "logical-1".to_owned(),
        turn_id: "turn-1".to_owned(),
        status: TurnStatus::Failed,
        created_at: 10,
        started_at: Some(10),
        completed_at: Some(12),
        error: Some(json!({ "code": "internal_error", "message": "boom" })),
        usage: None,
        subagent_id: None,
    };
    assert_eq!(
        placeholders(UPSERT_TURN_SQL),
        turn_params(&sample).expect("turn params").len()
    );
    assert_eq!(placeholders(SELECT_TURN_SQL), 2);
    assert_eq!(placeholders(SELECT_TURNS_ASC_SQL), 2);
    assert_eq!(
        placeholders(SELECT_TURNS_ASC_AFTER_SQL),
        5,
        "?2 is reused by the tie-break"
    );
    assert_eq!(placeholders(SELECT_TURNS_DESC_SQL), 2);
    assert_eq!(
        placeholders(SELECT_TURNS_DESC_AFTER_SQL),
        5,
        "?2 is reused by the tie-break"
    );
    assert_eq!(placeholders(CLOSE_OPEN_TURNS_SQL), 4);
    assert_eq!(placeholders(PATCH_SESSION_SQL), 11);

    for (sql, bound) in [
        (
            SELECT_TURNS_ASC_AFTER_SQL,
            "created_at > ?2 OR (created_at = ?2 AND turn_id > ?3)",
        ),
        (
            SELECT_TURNS_DESC_AFTER_SQL,
            "created_at < ?2 OR (created_at = ?2 AND turn_id < ?3)",
        ),
    ] {
        assert!(
            normalized(sql).contains(bound),
            "after is an exclusive (created_at, turn_id) keyset: {sql}"
        );
    }
    for sql in [
        SELECT_TURNS_ASC_SQL,
        SELECT_TURNS_ASC_AFTER_SQL,
        SELECT_TURNS_DESC_SQL,
        SELECT_TURNS_DESC_AFTER_SQL,
    ] {
        let sql = normalized(sql);
        assert!(
            sql.contains("ORDER BY created_at ASC, turn_id ASC")
                || sql.contains("ORDER BY created_at DESC, turn_id DESC"),
            "turns have a stable total order with a tie-break: {sql}"
        );
    }

    assert_eq!(placeholders(DELETE_SESSION_SQL), 1);
    assert_eq!(placeholders(DELETE_ITEMS_SQL), 1);
    assert_eq!(placeholders(DELETE_INPUTS_SQL), 1);
    assert_eq!(placeholders(DELETE_TURNS_SQL), 1);
    assert_eq!(placeholders(DELETE_BINDINGS_SQL), 1);
}

#[test]
fn turn_roundtrip_preserves_status_times_error_and_usage() {
    let sample = SwarmTurnRow {
        logical_id: "logical-1".to_owned(),
        turn_id: "turn-1".to_owned(),
        status: TurnStatus::Failed,
        created_at: 10,
        started_at: Some(11),
        completed_at: Some(12),
        error: Some(json!({ "code": "internal_error", "message": "boom" })),
        usage: Some(json!({ "input_tokens": 1, "output_tokens": 2 })),
        subagent_id: Some("sub-1".to_owned()),
    };
    let decoded = decode_turn(&turn_params(&sample).expect("params")).expect("decode");
    assert_eq!(decoded, sample);

    let mut open = sample.clone();
    open.status = TurnStatus::InProgress;
    open.completed_at = None;
    open.error = None;
    open.usage = None;
    open.subagent_id = None;
    let decoded = decode_turn(&turn_params(&open).expect("params")).expect("decode");
    assert_eq!(decoded, open);
}
