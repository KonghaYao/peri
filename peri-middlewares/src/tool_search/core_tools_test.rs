//! Tests for core_tools

use super::TOOL_READ;
use super::TOOL_WRITE;

#[test]
fn test_parse_extra_tool_call_requires_nonempty_name_and_object_params() {
    assert_eq!(
        super::parse_extra_tool_call(
            &serde_json::json!({"tool_name": "CronRegister", "params": {}})
        )
        .unwrap(),
        ("CronRegister".to_string(), serde_json::json!({}))
    );
    assert!(
        super::parse_extra_tool_call(&serde_json::json!({"tool_name": "", "params": {}})).is_err()
    );
    assert!(super::parse_extra_tool_call(
        &serde_json::json!({"tool_name": "CronRegister", "params": []})
    )
    .is_err());
}
#[test]
fn test_direct_tools_sorted_csv_is_stable_and_sorted() {
    let csv = super::direct_tools_sorted_csv([TOOL_WRITE, TOOL_READ, TOOL_WRITE]);
    assert_eq!(csv, "Read, Write");
}

#[test]
fn test_direct_tools_description_handles_empty_set() {
    assert_eq!(
        super::direct_tools_description(std::iter::empty()),
        "No other tools are directly available in this session."
    );
}
