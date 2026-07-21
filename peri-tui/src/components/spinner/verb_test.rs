//! Tests
use super::*;

#[test]
fn test_pick_verb_with_active_form() {
    let result = pick_verb(Some("搜索文件"));
    assert!(
        result.contains("搜索文件…"),
        "expected '搜索文件…', got '{}'",
        result
    );
}

#[test]
fn test_pick_verb_random() {
    let result = pick_verb(None);
    assert!(!result.is_empty(), "verb should not be empty");
    assert!(
        DEFAULT_VERBS.contains(&result.as_str()),
        "'{}' should be in DEFAULT_VERBS",
        result
    );
}
