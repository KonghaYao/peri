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
fn canonical_proxy_fields_parse_without_legacy_fields() {
    let call = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "arguments": {"expression": "* * * * *"}
    }))
    .unwrap();
    assert_eq!(call.target_name, "CronRegister");
    assert_eq!(
        call.arguments,
        serde_json::json!({"expression": "* * * * *"})
    );
}

#[test]
fn legacy_proxy_fields_normalize_to_the_same_call() {
    let canonical = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "arguments": {"value": 1}
    }))
    .unwrap();
    let legacy = super::parse_proxy_call(&serde_json::json!({
        "tool_name": "CronRegister",
        "params": {"value": 1}
    }))
    .unwrap();
    assert_eq!(canonical, legacy);
}

#[test]
fn duplicate_fields_with_equal_values_are_accepted_once() {
    let call = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "tool_name": "CronRegister",
        "arguments": {"value": 1},
        "params": {"value": 1}
    }))
    .unwrap();
    assert_eq!(call.target_name, "CronRegister");
    assert_eq!(call.arguments, serde_json::json!({"value": 1}));
}

#[test]
fn conflicting_name_fields_are_rejected() {
    let error = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "tool_name": "CronRemove",
        "arguments": {}
    }))
    .unwrap_err();
    assert!(error.contains("conflicting ToolInvoke name fields"));
}

#[test]
fn conflicting_arguments_fields_are_rejected() {
    let error = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "arguments": {"value": 1},
        "params": {"value": 2}
    }))
    .unwrap_err();
    assert!(error.contains("conflicting ToolInvoke arguments fields"));
}

#[test]
fn empty_name_and_non_object_arguments_are_rejected() {
    for input in [
        serde_json::json!({"name": "", "arguments": {}}),
        serde_json::json!({"name": 42, "arguments": {}}),
        serde_json::json!({"name": null, "arguments": {}}),
        serde_json::json!({"name": "", "tool_name": "CronRegister", "arguments": {}}),
        serde_json::json!({"name": "CronRegister"}),
        serde_json::json!({"name": "CronRegister", "arguments": []}),
        serde_json::json!({"name": "CronRegister", "arguments": null}),
    ] {
        assert!(super::parse_proxy_call(&input).is_err(), "{input}");
    }
}

#[test]
fn present_invalid_legacy_name_is_rejected_even_with_valid_canonical_name() {
    for input in [
        serde_json::json!({"name": "CronRegister", "tool_name": "", "arguments": {}}),
        serde_json::json!({"name": "CronRegister", "tool_name": 7, "arguments": {}}),
        serde_json::json!({"name": "CronRegister", "tool_name": null, "arguments": {}}),
    ] {
        assert!(super::parse_proxy_call(&input).is_err(), "{input}");
    }
}

#[test]
fn unknown_fields_are_ignored() {
    let call = super::parse_proxy_call(&serde_json::json!({
        "name": "CronRegister",
        "arguments": {},
        "server": "ignored",
        "command": "ignored"
    }))
    .unwrap();
    assert_eq!(call.target_name, "CronRegister");
}

#[test]
fn resolve_effective_name_unwraps_both_invoke_names() {
    for wrapper in [super::TOOL_INVOKE_NAME, super::EXECUTE_EXTRA_TOOL_NAME] {
        assert_eq!(
            super::resolve_effective_tool_name(
                wrapper,
                &serde_json::json!({"name": "Write", "arguments": {}})
            ),
            "Write"
        );
        assert_eq!(
            super::resolve_effective_tool_name(
                wrapper,
                &serde_json::json!({"tool_name": "Edit", "params": {}})
            ),
            "Edit"
        );
    }
}

#[test]
fn resolve_effective_name_fails_closed_on_conflicting_or_malformed_input() {
    for input in [
        serde_json::json!({"name": "Write", "tool_name": "Edit", "arguments": {}}),
        serde_json::json!({"name": "Write"}),
        serde_json::json!({"name": "", "arguments": {}}),
    ] {
        assert_eq!(
            super::resolve_effective_tool_name(super::TOOL_INVOKE_NAME, &input),
            super::TOOL_INVOKE_NAME,
            "malformed proxy input must not inherit the target's approval policy: {input}"
        );
    }
    assert_eq!(
        super::resolve_effective_tool_name("Read", &serde_json::json!({})),
        "Read"
    );
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
