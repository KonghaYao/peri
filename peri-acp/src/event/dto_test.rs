use super::*;

#[test]
fn test_compact_file_info_dto_roundtrip() {
    let dto = CompactFileInfoDto {
        path: "/tmp/foo.rs".to_string(),
        lines: 42,
    };
    let json = serde_json::to_string(&dto).unwrap();
    let back: CompactFileInfoDto = serde_json::from_str(&json).unwrap();
    assert_eq!(dto, back);
}

#[test]
fn test_workflow_progress_dto_roundtrip_minimal() {
    let dto = WorkflowProgressDto {
        run_id: "run-1".to_string(),
        workflow_name: "review".to_string(),
        event_type: "run_started".to_string(),
        agent_id: None,
        phase: None,
        label: None,
        agent_status: None,
        token_count: None,
        tool_count: None,
        run_status: None,
        message: None,
    };
    let json = serde_json::to_string(&dto).unwrap();
    // skip_serializing_if 应让 None 字段不出现在 JSON 中
    assert!(!json.contains("agent_id"));
    assert!(!json.contains("phase"));
    let back: WorkflowProgressDto = serde_json::from_str(&json).unwrap();
    assert_eq!(dto, back);
}

#[test]
fn test_workflow_progress_dto_roundtrip_full() {
    let dto = WorkflowProgressDto {
        run_id: "run-1".to_string(),
        workflow_name: "review".to_string(),
        event_type: "agent_done".to_string(),
        agent_id: Some(42),
        phase: Some("review".to_string()),
        label: Some("reviewer".to_string()),
        agent_status: Some("done".to_string()),
        token_count: Some(1234),
        tool_count: Some(5),
        run_status: None,
        message: Some("all good".to_string()),
    };
    let json = serde_json::to_string(&dto).unwrap();
    let back: WorkflowProgressDto = serde_json::from_str(&json).unwrap();
    assert_eq!(dto, back);
}
