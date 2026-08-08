use super::*;

#[test]
fn test_workflow_start_params_serializes_camel_case() {
    let params = WorkflowStartParams {
        run_id: "r1".into(),
        script: "code".into(),
        args: None,
        budget_total: None,
        max_concurrency: 3,
        resume: None,
        cwd: "/tmp".into(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert!(json.get("runId").is_some(), "runId 应存在，实际: {json}");
    assert!(json.get("maxConcurrency").is_some());
    assert!(json.get("budgetTotal").is_some());
    assert!(json.get("run_id").is_none());
    assert!(json.get("max_concurrency").is_none());
}

#[test]
fn test_agent_run_params_deserializes_camel_case() {
    let json = serde_json::json!({
        "runId": "r1",
        "agentId": 0,
        "prompt": "hello",
        "model": "haiku",
        "maxTokens": 8192,
        "agentType": "general-purpose",
        "allowedTools": ["Read", "Grep"],
    });
    let params: AgentRunParams = serde_json::from_value(json).unwrap();
    assert_eq!(params.run_id, "r1");
    assert_eq!(params.agent_id, 0);
    assert_eq!(params.model.as_deref(), Some("haiku"));
    assert_eq!(params.max_tokens, Some(8192));
    assert_eq!(params.agent_type.as_deref(), Some("general-purpose"));
}

#[test]
fn test_progress_event_run_started_camel_case() {
    let json = serde_json::json!({
        "type": "run_started",
        "runId": "r1",
        "workflowName": "test",
    });
    let event: ProgressEvent = serde_json::from_value(json).unwrap();
    match event {
        ProgressEvent::RunStarted {
            run_id,
            workflow_name,
            ..
        } => {
            assert_eq!(run_id, "r1");
            assert_eq!(workflow_name, "test");
        }
        _ => panic!("expected RunStarted"),
    }
}

#[test]
fn test_progress_event_agent_started_camel_case() {
    let json = serde_json::json!({
        "type": "agent_started",
        "runId": "r1",
        "agentId": 3,
        "label": "coder",
    });
    let event: ProgressEvent = serde_json::from_value(json).unwrap();
    match event {
        ProgressEvent::AgentStarted {
            run_id,
            agent_id,
            label,
            ..
        } => {
            assert_eq!(run_id, "r1");
            assert_eq!(agent_id, 3);
            assert_eq!(label.as_deref(), Some("coder"));
        }
        _ => panic!("expected AgentStarted"),
    }
}

#[test]
fn test_workflow_done_params_camel_case() {
    let json = serde_json::json!({
        "runId": "r1",
        "status": "completed",
        "returnValue": {"ok": true},
    });
    let done: WorkflowDoneParams = serde_json::from_value(json).unwrap();
    assert_eq!(done.run_id, "r1");
    assert_eq!(done.status, "completed");
    assert!(done.return_value.is_some());
}

#[test]
fn test_workflow_kill_params_camel_case() {
    let json = serde_json::json!({"runId": "r1"});
    let kill: WorkflowKillParams = serde_json::from_value(json).unwrap();
    assert_eq!(kill.run_id, "r1");
}
