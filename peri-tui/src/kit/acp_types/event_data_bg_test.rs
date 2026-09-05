use crate::kit::acp_types::AcpEventData;

#[test]
fn test_bg_task_completed_decodes_output_preview() {
    let data = serde_json::json!({
        "task_id": "t1",
        "success": true,
        "duration_ms": 100,
        "output_preview": "hello preview"
    });
    let ev = AcpEventData::decode("bg-task-completed", data);
    match ev {
        AcpEventData::BgTaskCompleted {
            output_preview,
            task_id,
            ..
        } => {
            assert_eq!(task_id, "t1");
            assert_eq!(output_preview.as_deref(), Some("hello preview"));
        }
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn test_bg_task_completed_missing_preview_still_ok() {
    let data = serde_json::json!({
        "task_id": "t2",
        "success": false,
        "duration_ms": 50
    });
    let ev = AcpEventData::decode("bg-task-completed", data);
    match ev {
        AcpEventData::BgTaskCompleted { output_preview, .. } => {
            assert!(output_preview.is_none());
        }
        other => panic!("unexpected variant {other:?}"),
    }
}
