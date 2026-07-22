use super::*;

#[test]
fn test_split_camel_case() {
    assert_eq!(split_camel_case("CronCreate"), vec!["cron", "create"]);
    assert_eq!(
        split_camel_case("SearchExtraTools"),
        vec!["search", "extra", "tools"]
    );
    assert_eq!(split_camel_case("Read"), vec!["read"]);
}

#[test]
fn test_split_mcp_prefix() {
    assert_eq!(
        split_mcp_prefix("mcp__slack__send_message"),
        vec!["slack", "send_message"]
    );
    assert_eq!(
        split_mcp_prefix("mcp__read_resource"),
        vec!["read_resource"]
    );
    assert_eq!(split_mcp_prefix("Read"), vec!["read"]);
}

#[test]
fn test_parse_query() {
    let (req, opt) = parse_query("+slack message");
    assert_eq!(req, vec!["slack"]);
    assert_eq!(opt, vec!["message"]);

    let (req, opt) = parse_query("cron create");
    assert_eq!(req, Vec::<String>::new());
    assert_eq!(opt, vec!["cron", "create"]);
}

#[test]
fn test_keyword_score_required_missing() {
    let score = keyword_score(
        "CronRegister",
        "Register a cron task",
        &["slack".to_string()],
        &[],
    );
    assert_eq!(score, 0.0);
}

#[test]
fn test_keyword_score_match() {
    let score = keyword_score(
        "CronRegister",
        "Register a scheduled cron task",
        &[],
        &["cron".to_string(), "register".to_string()],
    );
    assert!(score >= 1.0);
}
