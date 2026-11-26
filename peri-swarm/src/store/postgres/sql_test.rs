//! Statement-shape tests. No database is contacted.
//!
//! They pin the two properties a mechanical rewrite can silently break: `$n`
//! must cover exactly `1..=bind_count`, and no SQLite-ism of the Loom-era
//! adapter may survive.

use super::*;
use crate::domain::ItemOrder;

/// Every positional parameter index referenced by `sql`, in order of appearance.
fn placeholders(sql: &str) -> Vec<u32> {
    let bytes = sql.as_bytes();
    let mut indices = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] == b'$' && bytes.get(position + 1).is_some_and(u8::is_ascii_digit) {
            let start = position + 1;
            let mut end = start;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            indices.push(sql[start..end].parse().expect("placeholder index"));
            position = end;
        } else {
            position += 1;
        }
    }
    indices
}

/// `(name, statement, bind_count)` for every static statement.
fn statements() -> Vec<(&'static str, &'static str, usize)> {
    vec![
        ("UPSERT_SESSION", UPSERT_SESSION_SQL, 14),
        ("INSERT_SESSION", INSERT_SESSION_SQL, 14),
        ("SELECT_SESSION", SELECT_SESSION_SQL, 1),
        ("UPDATE_SESSION", UPDATE_SESSION_SQL, 4),
        ("PATCH_SESSION", PATCH_SESSION_SQL, 11),
        ("DELETE_SESSION", DELETE_SESSION_SQL, 1),
        ("DELETE_ITEMS", DELETE_ITEMS_SQL, 1),
        ("DELETE_INPUTS", DELETE_INPUTS_SQL, 1),
        ("DELETE_TURNS", DELETE_TURNS_SQL, 1),
        ("REASSIGN_BINDINGS", REASSIGN_BINDINGS_SQL, 1),
        ("DELETE_BINDINGS", DELETE_BINDINGS_SQL, 1),
        ("UPSERT_TURN", UPSERT_TURN_SQL, 9),
        ("SELECT_TURN", SELECT_TURN_SQL, 2),
        ("SELECT_TURNS_ASC", SELECT_TURNS_ASC_SQL, 2),
        ("SELECT_TURNS_ASC_AFTER", SELECT_TURNS_ASC_AFTER_SQL, 4),
        ("SELECT_TURNS_DESC", SELECT_TURNS_DESC_SQL, 2),
        ("SELECT_TURNS_DESC_AFTER", SELECT_TURNS_DESC_AFTER_SQL, 4),
        ("CLOSE_OPEN_TURNS", CLOSE_OPEN_TURNS_SQL, 4),
        ("HAS_OPEN_TURN", HAS_OPEN_TURN_SQL, 1),
        ("INSERT_ITEM", INSERT_ITEM_SQL, 8),
        ("SELECT_ITEMS_AFTER", SELECT_ITEMS_AFTER_SQL, 3),
        ("SELECT_ITEMS_BEFORE", SELECT_ITEMS_BEFORE_SQL, 3),
        ("SELECT_ITEMS_TAIL", SELECT_ITEMS_TAIL_SQL, 2),
        ("SELECT_ITEM_BY_ID", SELECT_ITEM_BY_ID_SQL, 2),
        ("SELECT_ITEM_PAGE_ASC", SELECT_ITEM_PAGE_ASC_SQL, 3),
        ("SELECT_ITEM_PAGE_DESC", SELECT_ITEM_PAGE_DESC_SQL, 3),
        ("SELECT_ITEM_PAGE_TAIL", SELECT_ITEM_PAGE_TAIL_SQL, 2),
        ("SELECT_LAST_SEQ", SELECT_LAST_SEQ_SQL, 1),
        ("INSERT_INPUT", INSERT_INPUT_SQL, 5),
        ("SELECT_INPUT", SELECT_INPUT_SQL, 2),
        ("UPSERT_BINDING", UPSERT_BINDING_SQL, 9),
        ("ATTACH_BINDING", ATTACH_BINDING_SQL, 6),
        ("SELECT_BINDING", SELECT_BINDING_SQL, 1),
    ]
}

#[test]
fn every_statement_binds_a_contiguous_parameter_range() {
    for (name, sql, binds) in statements() {
        let mut indices = placeholders(sql);
        indices.sort_unstable();
        indices.dedup();
        let expected: Vec<u32> = (1..=binds as u32).collect();
        assert_eq!(indices, expected, "{name} placeholders");
    }
}

#[test]
fn no_statement_keeps_a_sqlite_ism() {
    for (name, sql, _) in statements() {
        for forbidden in [
            "?1",
            "PRAGMA",
            "changes()",
            "json_each(",
            "sqlite_",
            "json_extract",
        ] {
            assert!(
                !sql.contains(forbidden),
                "{name} still contains {forbidden}: {sql}"
            );
        }
    }
}

#[test]
fn the_session_identity_guard_compares_root_and_cwd() {
    assert!(UPSERT_SESSION_SQL.contains("ON CONFLICT (logical_id) DO UPDATE SET"));
    assert!(UPSERT_SESSION_SQL
        .contains("WHERE swarm_sessions.root_session_id = excluded.root_session_id"));
    assert!(UPSERT_SESSION_SQL.contains("AND swarm_sessions.cwd = excluded.cwd"));
    // The plain insert has no conflict clause: a duplicate identity is a Conflict.
    assert!(!INSERT_SESSION_SQL.contains("ON CONFLICT"));
}

#[test]
fn inputs_are_insert_if_absent() {
    assert!(INSERT_INPUT_SQL.contains("ON CONFLICT (logical_id, input_id) DO NOTHING"));
}

#[test]
fn the_turn_upsert_keeps_the_opening_created_at() {
    let update = UPSERT_TURN_SQL
        .split("DO UPDATE SET")
        .nth(1)
        .expect("turn upsert has an update clause");
    assert!(update.contains("status = excluded.status"));
    assert!(
        !update.contains("created_at = excluded.created_at"),
        "the terminal write must keep the opening created_at: {update}"
    );
}

#[test]
fn item_pages_group_by_item_not_by_row() {
    for sql in [
        SELECT_ITEM_PAGE_ASC_SQL,
        SELECT_ITEM_PAGE_DESC_SQL,
        SELECT_ITEM_PAGE_TAIL_SQL,
    ] {
        assert!(sql.contains("GROUP BY logical_id, item_id"), "{sql}");
        assert!(sql.contains("MAX(seq)"), "{sql}");
        assert!(sql.contains("JOIN page p"), "{sql}");
    }
    assert!(SELECT_ITEM_PAGE_ASC_SQL.contains("HAVING MAX(seq) > $2"));
    assert!(SELECT_ITEM_PAGE_DESC_SQL.contains("HAVING MAX(seq) < $2"));
    // The tail page has no cursor parameter.
    assert!(!SELECT_ITEM_PAGE_TAIL_SQL.contains("HAVING"));
}

#[test]
fn the_metadata_filter_compares_key_and_value_as_text() {
    let sql = session_list_sql(ItemOrder::Asc, 2);
    assert!(
        sql.contains("jsonb_each_text(s.metadata_json::jsonb) m(key, value)"),
        "{sql}"
    );
    assert!(sql.contains("m.key = $3 AND m.value = $4"), "{sql}");
    assert!(sql.contains("m.key = $5 AND m.value = $6"), "{sql}");
    assert!(sql.contains("ORDER BY s.logical_id ASC LIMIT $2"), "{sql}");
    assert!(placeholders(&sql).iter().all(|index| *index <= 6));
}

#[test]
fn session_lists_honour_the_requested_order_and_cursor() {
    let asc = session_list_sql(ItemOrder::Asc, 0);
    assert!(asc.contains("s.logical_id > $1"), "{asc}");
    assert!(asc.contains("ORDER BY s.logical_id ASC"), "{asc}");
    let desc = session_list_sql(ItemOrder::Desc, 0);
    assert!(desc.contains("s.logical_id < $1"), "{desc}");
    assert!(desc.contains("ORDER BY s.logical_id DESC"), "{desc}");
}

#[test]
fn binding_hand_over_reads_the_environment_json_as_text() {
    assert!(
        REASSIGN_BINDINGS_SQL.contains("(s.environment_json::jsonb ->> 'type') = 'openai_hosted'")
    );
    assert!(REASSIGN_BINDINGS_SQL
        .contains("(s.environment_json::jsonb ->> 'id') = swarm_bindings.logical_env_id"));
}

#[test]
fn the_guarded_binding_update_matches_the_shared_environment() {
    assert!(ATTACH_BINDING_SQL.contains("WHERE logical_env_id = $1"));
    assert!(ATTACH_BINDING_SQL.contains("AND sandbox_id = $3"));
    assert!(ATTACH_BINDING_SQL.contains("AND workspace_path = $4"));
    assert!(ATTACH_BINDING_SQL.contains("last_run_id = COALESCE($5, last_run_id)"));
}

#[test]
fn the_patch_statement_decides_each_field_independently() {
    for flag in ["$2", "$4", "$6", "$8", "$10"] {
        assert!(
            PATCH_SESSION_SQL.contains(&format!("CASE WHEN {flag} THEN")),
            "missing {flag}: {PATCH_SESSION_SQL}"
        );
    }
    assert!(PATCH_SESSION_SQL.contains("ELSE model END"));
    assert!(PATCH_SESSION_SQL.contains("ELSE last_activity_at END"));
}

#[test]
fn the_placeholder_reader_sees_every_index() {
    assert_eq!(placeholders("SELECT $1, $2::text, '$3'"), vec![1, 2, 3]);
    assert!(placeholders("SELECT 'literal'").is_empty());
}
