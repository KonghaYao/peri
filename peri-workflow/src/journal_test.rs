use super::*;
use crate::protocol::{AgentRunResult, Usage};
use tempfile::TempDir;

fn make_store() -> (TempDir, WorkflowJournalStore) {
    let tmp = TempDir::new().unwrap();
    let store = WorkflowJournalStore::new(tmp.path().to_str().unwrap());
    (tmp, store)
}

#[test]
fn test_init_run_creates_dir_and_script() {
    let (_tmp, store) = make_store();
    store.init_run("run-1", "export const meta = {}").unwrap();
    let script = std::fs::read_to_string(store.run_dir("run-1").join("script.js")).unwrap();
    assert_eq!(script, "export const meta = {}");
}

#[test]
fn test_append_and_read_all_journal() {
    let (_tmp, store) = make_store();
    store.init_run("run-1", "script").unwrap();
    let entry = JournalEntry {
        key: "abc123".into(),
        seq: 0,
        result: AgentRunResult::Ok {
            output: "hello".into(),
            usage: Usage { output_tokens: 10 },
            model: None,
            tool_count: None,
            token_count: None,
        },
    };
    store.append("run-1", &entry).unwrap();
    let entries = store.read_all("run-1").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "abc123");
    assert_eq!(entries[0].seq, 0);
}

#[test]
fn test_truncate_clears_journal() {
    let (_tmp, store) = make_store();
    store.init_run("run-1", "script").unwrap();
    let entry = JournalEntry {
        key: "k".into(),
        seq: 0,
        result: AgentRunResult::Skipped,
    };
    store.append("run-1", &entry).unwrap();
    store.truncate("run-1").unwrap();
    let entries = store.read_all("run-1").unwrap();
    assert!(entries.is_empty());
}

#[test]
fn test_write_and_read_state() {
    let (_tmp, store) = make_store();
    store.init_run("run-1", "script").unwrap();
    let state = RunState {
        run_id: "run-1".into(),
        workflow_name: "test".into(),
        status: "completed".into(),
        return_value: Some(serde_json::json!({"ok": true})),
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: Some("2026-06-22T00:01:00Z".into()),
        error: None,
    };
    store.write_state("run-1", &state).unwrap();
    let read = store.read_state("run-1").unwrap();
    assert_eq!(read.run_id, "run-1");
    assert_eq!(read.status, "completed");
}

#[test]
fn test_state_error_field_round_trip() {
    // failed 的 state.json 必须能保存并读回 error 字段
    let (_tmp, store) = make_store();
    store.init_run("run-err", "script").unwrap();
    let state = RunState {
        run_id: "run-err".into(),
        workflow_name: "test".into(),
        status: "failed".into(),
        return_value: None,
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: Some("2026-06-22T00:00:01Z".into()),
        error: Some("parallel thunk #0 failed: t is not a function".into()),
    };
    store.write_state("run-err", &state).unwrap();
    let read = store.read_state("run-err").unwrap();
    assert_eq!(read.status, "failed");
    assert_eq!(
        read.error.as_deref(),
        Some("parallel thunk #0 failed: t is not a function")
    );
}

#[test]
fn test_state_error_skipped_when_none() {
    // error 为 None 时序列化应省略该字段（向后兼容旧 state.json）
    let (_tmp, store) = make_store();
    store.init_run("run-ok", "script").unwrap();
    let state = RunState {
        run_id: "run-ok".into(),
        workflow_name: "test".into(),
        status: "completed".into(),
        return_value: None,
        script: "script".into(),
        started_at: "2026-06-22T00:00:00Z".into(),
        finished_at: None,
        error: None,
    };
    store.write_state("run-ok", &state).unwrap();
    let raw = std::fs::read_to_string(store.run_dir("run-ok").join("state.json")).unwrap();
    assert!(!raw.contains("error"), "None 时不应序列化 error 字段");
}
