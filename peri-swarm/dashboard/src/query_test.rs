use serde_json::json;

use super::*;

#[test]
fn empty_and_whitespace_only_statements_are_rejected() {
    assert_eq!(validate_sql(""), Err(SqlError::Empty));
    assert_eq!(validate_sql("   \n\t "), Err(SqlError::Empty));
}

#[test]
fn select_is_case_insensitive_and_leading_whitespace_is_allowed() {
    assert_eq!(validate_sql("SELECT 1"), Ok(()));
    assert_eq!(validate_sql("select * from \"peri_swarm_logs\""), Ok(()));
    assert_eq!(validate_sql("  SeLeCt name FROM logs"), Ok(()));
    assert_eq!(validate_sql("\n\t SELECT 1\n"), Ok(()));
}

#[test]
fn exactly_one_trailing_semicolon_is_allowed() {
    assert_eq!(validate_sql("SELECT 1;"), Ok(()));
    assert_eq!(validate_sql("SELECT 1;   \n"), Ok(()));
    assert_eq!(validate_sql("  SELECT 1 ;  "), Ok(()));
}

#[test]
fn inner_or_repeated_semicolons_are_rejected() {
    assert_eq!(validate_sql("SELECT 1; SELECT 2"), Err(SqlError::Semicolon));
    assert_eq!(validate_sql("SELECT 1;;"), Err(SqlError::Semicolon));
    assert_eq!(validate_sql("SELECT ';'"), Err(SqlError::Semicolon));
    assert_eq!(validate_sql(";SELECT 1"), Err(SqlError::Semicolon));
}

#[test]
fn statements_over_eight_kib_are_rejected() {
    let mut sql = String::from("SELECT ");
    sql.push_str(&"a".repeat(MAX_SQL_BYTES - sql.len()));
    assert_eq!(sql.len(), MAX_SQL_BYTES);
    assert_eq!(validate_sql(&sql), Ok(()));

    sql.push('b');
    assert_eq!(sql.len(), MAX_SQL_BYTES + 1);
    assert_eq!(validate_sql(&sql), Err(SqlError::TooLarge));
}

#[test]
fn nul_bytes_are_rejected() {
    assert_eq!(validate_sql("SELECT 1\0"), Err(SqlError::Nul));
    assert_eq!(validate_sql("SELECT\0 1"), Err(SqlError::Nul));
}

#[test]
fn non_select_statements_are_rejected() {
    for sql in [
        "DELETE FROM logs",
        "update logs set status = 500",
        "SELECTED",
        "WITH rows AS (SELECT 1) SELECT * FROM rows",
        // A lone semicolon is not a statement.
        ";",
    ] {
        assert_eq!(validate_sql(sql), Err(SqlError::NotSelect), "{sql}");
    }
}

#[test]
fn columns_prefer_reported_names_then_row_keys() {
    let hits = vec![json!({"b": 1, "a": 2}), json!({"c": 3, "a": 4})];
    let reported = vec!["b".to_owned(), " b ".to_owned(), "  ".to_owned()];
    // Reported names win outright; row keys are not appended.
    assert_eq!(resolve_columns(&reported, &hits), ["b"]);

    // Without reported names the union is sorted, so it does not depend on
    // `serde_json`'s map implementation or workspace feature unification.
    assert_eq!(resolve_columns(&[], &hits), ["a", "b", "c"]);
    // Non-object rows contribute nothing and never panic.
    assert_eq!(
        resolve_columns(&[], &[json!(1), json!(null)]),
        Vec::<String>::new()
    );
}

#[test]
fn top_level_limit_and_offset_disable_server_paging_only() {
    for sql in [
        "SELECT * FROM \"peri_swarm_logs\" LIMIT 20",
        "select * from logs limit 20 offset 40",
        "SELECT * FROM logs OFFSET 10",
        "SELECT * FROM logs\tLIMIT\t5",
        "SELECT * FROM logs -- trailing note\nLIMIT 5",
        "SELECT * FROM logs /* note */ LIMIT 5",
    ] {
        assert!(has_top_level_row_limit(sql), "{sql}");
    }

    for sql in [
        "SELECT * FROM \"peri_swarm_logs\"",
        "SELECT 'limit' FROM logs",
        "SELECT \"limit\" FROM logs",
        "SELECT `offset` FROM logs",
        "SELECT * FROM logs WHERE note = 'LIMIT 5'",
        "SELECT * FROM logs /* LIMIT 5 */",
        "SELECT * FROM logs -- LIMIT 5",
        // A subquery limit is part of the row definition, not the page size.
        "SELECT * FROM (SELECT * FROM logs LIMIT 5)",
        "SELECT * FROM logs WHERE id IN (SELECT id FROM t LIMIT 3)",
        "SELECT limited, offsetting FROM logs",
    ] {
        assert!(!has_top_level_row_limit(sql), "{sql}");
    }

    // The validator still accepts a statement with its own limit.
    assert_eq!(validate_sql("SELECT 1 LIMIT 20"), Ok(()));
}

#[test]
fn next_page_needs_a_full_page_and_the_row_cap() {
    assert_eq!(page_offset(0), 0);
    assert_eq!(page_offset(4), 80);

    assert!(has_next_page(0, QUERY_PAGE_SIZE));
    assert!(has_next_page(3, QUERY_PAGE_SIZE));
    assert!(!has_next_page(4, QUERY_PAGE_SIZE));
    assert!(!has_next_page(0, QUERY_PAGE_SIZE - 1));
    // Defensive: a server that returns more rows than requested cannot page.
    assert!(!has_next_page(0, QUERY_PAGE_SIZE + 1));
    assert_eq!(MAX_QUERY_ROWS, QUERY_PAGE_SIZE * 5);
}
