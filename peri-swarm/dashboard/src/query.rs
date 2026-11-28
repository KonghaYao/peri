//! Rules for the ad-hoc read-only Query page.
//!
//! The dashboard's dedicated OpenObserve account is an admin and **can write**
//! (OSS v1.2.0 has no viewer role). [`validate_sql`] keeps an accidental
//! `DELETE`, `UPDATE` or multi-statement paste from being sent, but it is an
//! operator-safety check, **not** a security boundary: the account itself
//! remains writable, and any client holding the same credentials could write
//! directly. Protection depends on the private network, the dedicated account
//! and the `0600` credential file.
//!
//! Validation never echoes the statement, so an error message is always safe to
//! display and log.

use serde_json::Value;

/// Largest statement the Query page accepts, in UTF-8 bytes.
pub const MAX_SQL_BYTES: usize = 8 * 1024;
/// Rows requested per Query page.
pub const QUERY_PAGE_SIZE: usize = 20;
/// Most rows the Query page reads before it stops paging.
pub const MAX_QUERY_ROWS: usize = 100;

/// Why a statement was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlError {
    /// The statement is empty or only whitespace.
    Empty,
    /// The statement is longer than [`MAX_SQL_BYTES`].
    TooLarge,
    /// The first token is not `SELECT`.
    NotSelect,
    /// A semicolon appears anywhere except as the final character.
    Semicolon,
    /// The text contains a NUL byte.
    Nul,
}

/// Accept one single `SELECT` statement.
///
/// Rules, in order:
/// 1. the trimmed text is non-empty;
/// 2. the UTF-8 byte length is at most [`MAX_SQL_BYTES`];
/// 3. no NUL byte appears;
/// 4. at most one `;` exists, and only as the final character;
/// 5. the first whitespace-separated token is `SELECT` (ASCII case
///    insensitive).
///
/// Semicolons inside string literals are rejected too: the check is a cheap
/// operator guard, not a SQL parser. Statements starting with `WITH` are
/// rejected for the same reason.
///
/// # Errors
///
/// Returns the specific [`SqlError`] when the statement breaks a rule.
pub fn validate_sql(sql: &str) -> Result<(), SqlError> {
    if sql.trim().is_empty() {
        return Err(SqlError::Empty);
    }
    if sql.len() > MAX_SQL_BYTES {
        return Err(SqlError::TooLarge);
    }
    if sql.contains('\0') {
        return Err(SqlError::Nul);
    }
    let trimmed = sql.trim();
    let body = trimmed.strip_suffix(';').unwrap_or(trimmed).trim_end();
    if body.contains(';') {
        return Err(SqlError::Semicolon);
    }
    let first = body.split_whitespace().next().unwrap_or_default();
    if !first.eq_ignore_ascii_case("SELECT") {
        return Err(SqlError::NotSelect);
    }
    Ok(())
}

/// Column names for a result page.
///
/// Server-reported names win, in server order. When the server omits
/// `columns`, the keys of every row form the fallback, in first-appearance
/// order. The order is stable in both cases.
#[must_use]
pub fn resolve_columns(reported: &[String], hits: &[Value]) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    if !reported.is_empty() {
        for name in reported {
            push_column(&mut columns, name);
        }
        return columns;
    }
    for hit in hits {
        let Some(object) = hit.as_object() else {
            continue;
        };
        for name in object.keys() {
            push_column(&mut columns, name);
        }
    }
    columns
}

fn push_column(columns: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty() || columns.iter().any(|column| column == name) {
        return;
    }
    columns.push(name.to_owned());
}

/// Requested row offset for a zero-based page.
#[must_use]
pub fn page_offset(page: usize) -> u64 {
    u64::try_from(page)
        .unwrap_or(u64::MAX)
        .saturating_mul(QUERY_PAGE_SIZE as u64)
}

/// Whether a further page may exist.
///
/// OpenObserve's `total` is the row count of the returned page, not a global
/// match count, so the only honest signal is the number of rows on the current
/// page. A full page enables Next until [`MAX_QUERY_ROWS`] rows were read.
#[must_use]
pub fn has_next_page(page: usize, rows_on_page: usize) -> bool {
    rows_on_page == QUERY_PAGE_SIZE
        && page_offset(page).saturating_add(QUERY_PAGE_SIZE as u64) < MAX_QUERY_ROWS as u64
}

#[cfg(test)]
#[path = "query_test.rs"]
mod tests;
