use serde_json::json;

use super::*;
use crate::state::{
    DEFAULT_DEADLINE_MS, DEFAULT_MAX_BYTES, DEFAULT_MAX_EVENTS, DEFAULT_READ_BYTES,
    DEFAULT_READ_LIMIT,
};

fn map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().cloned().expect("object")
}

#[test]
fn start_arguments_apply_documented_defaults() {
    let parsed = parse_start(&map(json!({ "command": "tail -f app.log" }))).expect("valid");
    assert_eq!(parsed.command, "tail -f app.log");
    assert!(parsed.cwd.is_none());
    assert!(parsed.label.is_none());
    assert!(parsed.deadline_ms.is_none());
    assert!(parsed.max_events.is_none());
    assert!(parsed.max_bytes.is_none());

    let resolved = crate::state::resolve_start(&parsed, "/workspace").expect("resolved");
    assert_eq!(resolved.deadline.as_millis() as u64, DEFAULT_DEADLINE_MS);
    assert_eq!(resolved.max_events, DEFAULT_MAX_EVENTS);
    assert_eq!(resolved.max_bytes, DEFAULT_MAX_BYTES);
}

#[test]
fn start_arguments_reject_missing_command_and_wrong_types() {
    assert!(parse_start(&map(json!({}))).is_err());
    assert!(parse_start(&map(json!({ "command": "   " }))).is_err());
    assert!(parse_start(&map(json!({ "command": "true", "cwd": 3 }))).is_err());
    assert!(parse_start(&map(json!({ "command": "true", "deadline_ms": "soon" }))).is_err());
    assert!(parse_start(&map(json!({ "command": "true", "max_events": 1.5 }))).is_err());
}

#[test]
fn read_arguments_apply_defaults_and_bounds() {
    let parsed = parse_read(&map(json!({ "task_id": "monitor-1" }))).expect("valid");
    assert_eq!(parsed.task_id, "monitor-1");
    assert!(parsed.after_seq.is_none());
    assert_eq!(parsed.limit, DEFAULT_READ_LIMIT);
    assert_eq!(parsed.max_bytes, DEFAULT_READ_BYTES);

    let parsed = parse_read(&map(json!({
        "task_id": "monitor-1",
        "after_seq": 7,
        "limit": 3,
        "max_bytes": 1024
    })))
    .expect("valid");
    assert_eq!(parsed.after_seq, Some(7));
    assert_eq!(parsed.limit, 3);
    assert_eq!(parsed.max_bytes, 1024);

    assert!(parse_read(&map(json!({ "task_id": "x", "limit": 0 }))).is_err());
    assert!(parse_read(&map(json!({ "task_id": "x", "limit": 501 }))).is_err());
    assert!(parse_read(&map(json!({ "task_id": "x", "max_bytes": 0 }))).is_err());
    assert!(parse_read(&map(json!({ "task_id": "x" , "after_seq": -1 }))).is_err());
}

#[test]
fn list_arguments_apply_defaults_and_bounds() {
    let parsed = parse_list(&map(json!({}))).expect("valid");
    assert!(!parsed.include_ended);
    assert_eq!(parsed.limit, 50);
    assert!(
        parse_list(&map(json!({ "include_ended": true })))
            .unwrap()
            .include_ended
    );
    assert!(parse_list(&map(json!({ "limit": 0 }))).is_err());
    assert!(parse_list(&map(json!({ "limit": 201 }))).is_err());
    assert!(parse_list(&map(json!({ "include_ended": "yes" }))).is_err());
}

#[test]
fn stop_arguments_require_task_id() {
    assert!(parse_stop(&map(json!({}))).is_err());
    let parsed = parse_stop(&map(json!({
        "task_id": "monitor-1",
        "reason": "done"
    })))
    .expect("valid");
    assert_eq!(parsed.task_id, "monitor-1");
    assert_eq!(parsed.reason.as_deref(), Some("done"));
}

#[test]
fn declared_tools_are_deferred_and_in_registry_order() {
    let state = Arc::new(MonitorState::new("."));
    let tools = tools(state);
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(
        names,
        vec![
            "monitor_start",
            "monitor_read",
            "monitor_stop",
            "monitor_list"
        ]
    );
    assert!(tools.iter().all(|tool| !tool.is_direct()));
}
