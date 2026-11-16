use serde_json::json;

// === matcher tests ===

#[test]
fn test_matcher_wildcard() {
    assert!(super::matches_matcher("*", "Bash"));
    assert!(super::matches_matcher("*", "Write"));
    assert!(super::matches_matcher("", "Bash"));
}

#[test]
fn test_matcher_exact() {
    assert!(super::matches_matcher("Write", "Write"));
    assert!(!super::matches_matcher("Write", "Edit"));
    assert!(!super::matches_matcher("Bash", "bash")); // case sensitive
}

#[test]
fn test_matcher_pipe_list() {
    assert!(super::matches_matcher("Write|Edit|Grep", "Grep"));
    assert!(super::matches_matcher("Write|Edit|Grep", "Write"));
    assert!(!super::matches_matcher("Write|Edit", "Grep"));
}

#[test]
fn test_matcher_regex() {
    assert!(super::matches_matcher("^Bash.*", "Bash -c echo"));
    assert!(!super::matches_matcher("^Bash", "Write"));
    assert!(super::matches_matcher(".*Edit.*", "EditFile"));
}

#[test]
fn test_matcher_invalid_regex() {
    assert!(!super::matches_matcher("[invalid", "Write")); // regex compile fails → false
}

// === if condition tests ===

#[test]
fn test_if_condition_tool_name_match() {
    assert!(super::matches_if_condition(
        "Bash(git)",
        "Bash",
        &json!({"command": "git commit"})
    ));
}

#[test]
fn test_if_condition_tool_name_mismatch() {
    assert!(!super::matches_if_condition(
        "Bash(git)",
        "Write",
        &json!({"path": "file.txt"})
    ));
}

#[test]
fn test_if_condition_empty_rule() {
    assert!(super::matches_if_condition("Bash()", "Bash", &json!({})));
}

#[test]
fn test_if_condition_content_contains() {
    assert!(super::matches_if_condition(
        "Bash(git commit)",
        "Bash",
        &json!({"command": "git commit -m msg"})
    ));
}

#[test]
fn test_if_condition_content_not_contains() {
    assert!(!super::matches_if_condition(
        "Bash(git)",
        "Bash",
        &json!({"command": "npm install"})
    ));
}

// === parse_permission_rule tests ===

#[test]
fn test_parse_permission_rule_valid() {
    let (tool, rule) = super::parse_permission_rule("Bash(git commit)").unwrap();
    assert_eq!(tool, "Bash");
    assert_eq!(rule, "git commit");
}

#[test]
fn test_parse_permission_rule_empty_rule() {
    let (tool, rule) = super::parse_permission_rule("Write()").unwrap();
    assert_eq!(tool, "Write");
    assert_eq!(rule, "");
}

#[test]
fn test_parse_permission_rule_invalid() {
    assert!(super::parse_permission_rule("no_parens").is_none());
    assert!(super::parse_permission_rule(")(invalid(").is_none());
}
