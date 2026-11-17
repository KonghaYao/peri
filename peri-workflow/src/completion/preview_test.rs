use super::*;
use serde_json::json;
use tempfile::TempDir;

fn fixture() -> (TempDir, WorkflowJournalStore) {
    let tmp = TempDir::new().unwrap();
    let store = WorkflowJournalStore::new(tmp.path().to_str().unwrap());
    store.init_run("run-1", "script").unwrap();
    (tmp, store)
}

#[test]
fn complete_preview_restores_nested_externalized_results() {
    let (_tmp, store) = fixture();
    let report = "Verified the current artifact. ".repeat(20);
    store.write_output("run-1", "report", &report).unwrap();
    let value = json!({"checks": [true, null, 2, {"report": "${report}"}], "summary": "done"});
    let preview = result_preview(&value, &store, "run-1");
    assert!(preview.complete);
    assert_eq!(
        serde_json::from_str::<Value>(&preview.text).unwrap(),
        json!({"checks": [true, null, 2, {"report": report}], "summary": "done"})
    );
}

#[test]
fn truncation_is_utf8_safe_and_never_claims_a_complete_result() {
    let (_tmp, store) = fixture();
    for value in [json!("结果".repeat(10_000)), json!(["x".repeat(20_000)])] {
        let preview = result_preview(&value, &store, "run-1");
        assert!(!preview.complete);
        assert!(preview.text.len() <= WORKFLOW_RESULT_PREVIEW_BYTES);
        assert!(std::str::from_utf8(preview.text.as_bytes()).is_ok());
    }
    store
        .write_output("run-1", "large", &"结果".repeat(10_000))
        .unwrap();
    let preview = result_preview(&json!({"report": "${large}"}), &store, "run-1");
    assert!(!preview.complete);
    assert!(preview.text.len() <= WORKFLOW_RESULT_PREVIEW_BYTES);
}

#[test]
fn escaped_output_counts_towards_the_serialized_budget() {
    let (_tmp, store) = fixture();
    store
        .write_output("run-1", "escaped", &"\n\t\"\\".repeat(2_000))
        .unwrap();
    let preview = result_preview(&json!({"report": "${escaped}"}), &store, "run-1");
    assert!(!preview.complete);
    assert_eq!(preview.text.len(), WORKFLOW_RESULT_PREVIEW_BYTES);
}

#[test]
fn missing_unsafe_and_invalid_utf8_references_remain_explicitly_incomplete() {
    let (_tmp, store) = fixture();
    store.write_output("run-1", "invalid", "temporary").unwrap();
    std::fs::write(store.run_dir("run-1").join("outputs/invalid.txt"), [0xff]).unwrap();
    for reference in ["${absent}", "${../outside}", "${a/b}", "${}", "${invalid}"] {
        let value = json!({"report": reference});
        let preview = result_preview(&value, &store, "run-1");
        assert!(!preview.complete, "{reference}");
        assert_eq!(serde_json::from_str::<Value>(&preview.text).unwrap(), value);
    }
}

#[test]
fn externalized_text_is_not_recursively_interpreted_as_another_reference() {
    let (_tmp, store) = fixture();
    store
        .write_output("run-1", "literal", "${not-a-reference}")
        .unwrap();
    let preview = result_preview(&json!({"report": "${literal}"}), &store, "run-1");
    assert!(preview.complete);
    assert_eq!(
        serde_json::from_str::<Value>(&preview.text).unwrap(),
        json!({"report": "${not-a-reference}"})
    );
}

#[cfg(unix)]
#[test]
fn symlinked_outputs_cannot_supply_a_complete_preview() {
    use std::os::unix::fs::symlink;
    let (tmp, store) = fixture();
    let outside = tmp.path().join("private.txt");
    std::fs::write(&outside, "private content").unwrap();
    let outputs = store.run_dir("run-1").join("outputs");
    std::fs::create_dir_all(&outputs).unwrap();
    symlink(&outside, outputs.join("link.txt")).unwrap();
    let preview = result_preview(&json!({"report": "${link}"}), &store, "run-1");
    assert!(!preview.complete);
    assert!(!preview.text.contains("private content"));

    std::fs::remove_dir_all(&outputs).unwrap();
    symlink(tmp.path(), &outputs).unwrap();
    let preview = result_preview(&json!({"report": "${private}"}), &store, "run-1");
    assert!(!preview.complete);
    assert!(!preview.text.contains("private content"));
}

#[test]
fn exact_serialized_limit_remains_complete() {
    let (_tmp, store) = fixture();
    let preview = result_preview(
        &json!("x".repeat(WORKFLOW_RESULT_PREVIEW_BYTES - 2)),
        &store,
        "run-1",
    );
    assert!(preview.complete);
    assert_eq!(preview.text.len(), WORKFLOW_RESULT_PREVIEW_BYTES);
}
