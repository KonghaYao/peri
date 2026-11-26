//! PostgreSQL uses the same logical tables, constraints and indexes as the
//! local store. The driver-specific clauses below select 64-bit integers and
//! an explicit insertion-order column instead of SQLite's implicit rowid.

use super::sql::StatementSpec;
use crate::sessions::canonical;

pub(super) fn initialization_plan() -> Vec<StatementSpec> {
    canonical::PG_CREATE_TABLES
        .iter()
        .chain(canonical::PG_CREATE_INDEXES)
        .map(|sql| StatementSpec::bare(sql))
        .collect()
}

/// Used by both the initial collision guard and the read-only catalog check.
/// All names come from trusted DDL owned by the adapter.
pub(super) fn reserved_objects() -> String {
    canonical::CANONICAL_TABLES
        .iter()
        .copied()
        .chain(["peri_store_meta", super::ledger::OP_LEDGER_TABLE])
        .chain(canonical::INDEX_NAMES.iter().copied())
        .collect::<Vec<_>>()
        .join(",")
}
