//! Schema-decision tests. No database is contacted.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    adopt_refusal, missing_columns, require_supported_meta, MetaRow, CREATE_INDEX_SQL,
    CREATE_META_TABLE_SQL, CREATE_TABLE_SQL, EXPECTED_COLUMNS, SWARM_STATE_CONTRACT,
    SWARM_STATE_ENGINE, SWARM_STATE_SCHEMA_VERSION,
};
use crate::store::SwarmStateError;

fn meta(version: i64, engine: &str, contract: &str) -> MetaRow {
    MetaRow {
        version,
        engine: engine.to_owned(),
        contract: contract.to_owned(),
    }
}

fn current() -> MetaRow {
    meta(
        SWARM_STATE_SCHEMA_VERSION,
        SWARM_STATE_ENGINE,
        SWARM_STATE_CONTRACT,
    )
}

#[test]
fn the_owned_identity_is_version_one_on_postgres() {
    assert_eq!(SWARM_STATE_SCHEMA_VERSION, 1);
    assert_eq!(SWARM_STATE_ENGINE, "postgres");
    assert_eq!(SWARM_STATE_CONTRACT, "peri.swarm.postgres/v1");
    require_supported_meta(&current()).expect("this build's identity is supported");
}

#[test]
fn a_newer_version_fails_closed_with_a_repair_hint() {
    let error = require_supported_meta(&meta(2, SWARM_STATE_ENGINE, SWARM_STATE_CONTRACT))
        .expect_err("version 2 is not this build's");
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
    let message = error.to_string();
    for hint in [
        "version 2",
        "expected 1",
        "PERI_SWARM_STATE_STORE",
        "README",
    ] {
        assert!(message.contains(hint), "missing {hint}: {message}");
    }
}

#[test]
fn an_older_version_fails_closed_too() {
    let error = require_supported_meta(&meta(0, SWARM_STATE_ENGINE, SWARM_STATE_CONTRACT))
        .expect_err("version 0 is not this build's");
    assert!(
        matches!(
            error,
            SwarmStateError::SchemaVersion {
                found: 0,
                expected: 1
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_foreign_engine_or_contract_is_a_schema_error() {
    let engine =
        require_supported_meta(&meta(1, "loom", SWARM_STATE_CONTRACT)).expect_err("foreign engine");
    assert!(
        matches!(&engine, SwarmStateError::Schema(detail) if detail.contains("engine=\"loom\"")),
        "{engine:?}"
    );
    let contract = require_supported_meta(&meta(1, SWARM_STATE_ENGINE, "peri.swarm.sqlite/v1"))
        .expect_err("foreign contract");
    assert!(
        matches!(&contract, SwarmStateError::Schema(detail) if detail.contains("peri.swarm.sqlite/v1")),
        "{contract:?}"
    );
}

#[test]
fn a_non_empty_schema_without_meta_is_refused() {
    let error = adopt_refusal(&["foreign_table".to_owned(), "other".to_owned()]);
    let message = error.to_string();
    assert!(message.contains("not empty"), "{message}");
    assert!(message.contains("foreign_table, other"), "{message}");
    assert!(message.contains("swarm_meta"), "{message}");
    assert!(message.contains("PERI_SWARM_STATE_STORE"), "{message}");
}

#[test]
fn the_peri_session_schema_gets_its_own_refusal() {
    let error = adopt_refusal(&["peri_store_meta".to_owned(), "threads".to_owned()]);
    let message = error.to_string();
    assert!(message.contains("Peri session store"), "{message}");
    assert!(message.contains("different database"), "{message}");
    assert!(!message.contains("not empty"), "{message}");
}

#[test]
fn a_long_relation_list_is_truncated() {
    let relations: Vec<String> = (0..12).map(|index| format!("table_{index}")).collect();
    let message = adopt_refusal(&relations).to_string();
    assert!(message.contains("table_7"), "{message}");
    assert!(!message.contains("table_8"), "{message}");
    assert!(message.contains('…'), "{message}");
}

#[test]
fn the_shape_check_reports_every_missing_column() {
    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (table, columns) in EXPECTED_COLUMNS {
        actual.insert(
            table.to_owned(),
            columns.iter().map(|column| (*column).to_owned()).collect(),
        );
    }
    assert!(missing_columns(&actual).is_empty());

    actual
        .get_mut("swarm_items")
        .expect("items")
        .remove("truncated");
    actual.remove("swarm_turns");
    let missing = missing_columns(&actual);
    assert_eq!(
        missing,
        vec![
            "swarm_items.truncated".to_owned(),
            "swarm_turns.logical_id".to_owned(),
            "swarm_turns.turn_id".to_owned(),
            "swarm_turns.status".to_owned(),
            "swarm_turns.created_at".to_owned(),
            "swarm_turns.started_at".to_owned(),
            "swarm_turns.completed_at".to_owned(),
            "swarm_turns.error_json".to_owned(),
            "swarm_turns.usage_json".to_owned(),
            "swarm_turns.subagent_id".to_owned(),
        ]
    );
}

#[test]
fn the_expected_shape_covers_six_tables() {
    assert_eq!(EXPECTED_COLUMNS.len(), 6);
    assert_eq!(CREATE_TABLE_SQL.len(), 5);
    assert_eq!(CREATE_INDEX_SQL.len(), 2);
}

#[test]
fn every_owned_statement_is_idempotent() {
    assert!(CREATE_META_TABLE_SQL.contains("CREATE TABLE IF NOT EXISTS swarm_meta"));
    for sql in CREATE_TABLE_SQL {
        assert!(sql.starts_with("CREATE TABLE IF NOT EXISTS"), "{sql}");
    }
    for sql in CREATE_INDEX_SQL {
        assert!(sql.starts_with("CREATE INDEX IF NOT EXISTS"), "{sql}");
    }
}

#[test]
fn the_meta_table_is_a_singleton() {
    assert!(CREATE_META_TABLE_SQL.contains("singleton  SMALLINT PRIMARY KEY"));
    assert!(CREATE_META_TABLE_SQL.contains("CHECK (singleton = 0)"));
    assert!(CREATE_META_TABLE_SQL.contains("version    BIGINT NOT NULL"));
    assert!(CREATE_META_TABLE_SQL.contains("engine     TEXT   NOT NULL"));
    assert!(CREATE_META_TABLE_SQL.contains("contract   TEXT   NOT NULL"));
}

#[test]
fn item_ids_are_not_unique_but_seq_is() {
    let items = CREATE_TABLE_SQL
        .iter()
        .find(|sql| sql.contains("swarm_items"))
        .expect("items table");
    assert!(items.contains("PRIMARY KEY (logical_id, seq)"));
    assert!(
        !items.contains("UNIQUE"),
        "the item_id column must not carry a unique constraint: {items}"
    );
    assert!(CREATE_INDEX_SQL[0].contains("swarm_items_by_id ON swarm_items (logical_id, item_id)"));
}

#[test]
fn a_missing_authoritative_table_is_reported_not_repaired() {
    // `verify_authoritative_tables` runs on an initialized database; its input is
    // the catalog. Only `swarm_meta` describes a schema this build may repair
    // additively, so every other missing table is data loss.
    let tables: Vec<String> = ["swarm_meta", "swarm_sessions", "swarm_items"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        super::missing_tables(&tables),
        vec!["swarm_inputs", "swarm_bindings", "swarm_turns"],
        "the missing tables are named instead of being re-created"
    );
    let complete: Vec<String> = EXPECTED_COLUMNS
        .iter()
        .map(|(table, _)| (*table).to_owned())
        .collect();
    assert!(super::missing_tables(&complete).is_empty());
}
