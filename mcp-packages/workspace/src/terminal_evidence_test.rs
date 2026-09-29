use super::*;

#[tokio::test]
async fn test_bash_normal_command() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo hello"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn test_bash_typed_evidence_preserves_nonzero_exit() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let output = tool
        .invoke_output(
            serde_json::json!({"command": output_command(0, "tail-failure", 7)}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let evidence = output.execution.expect("Bash must report wait evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Failed);
    assert_eq!(evidence.exit_code, Some(7));
    assert!(output.text.contains("tail-failure"));
}

#[tokio::test]
async fn test_bash_typed_evidence_persists_10k_to_65k_projection() {
    let fixture = tempfile::tempdir().unwrap();
    let tool = BashTool::new(fixture.path().to_str().unwrap());
    let output = tool
        .invoke_output(
            serde_json::json!({
                "command": output_command(20000, "", 0)
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let evidence = output.execution.expect("Bash must report wait evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Completed);
    assert!(evidence.output_truncated);
    let path = evidence.output_ref.expect("full output reference");
    let full = std::fs::read_to_string(path).unwrap();
    assert!(full.len() > 10_000);
    assert!(output.text.chars().count() <= 10_000);
}

#[cfg(unix)]
#[tokio::test]
async fn test_bash_typed_evidence_marks_explicit_background_running() {
    let manager = Arc::new(TaskManager::new());
    let tool =
        BashTool::new(std::env::temp_dir().to_str().unwrap()).with_task_manager(manager.clone());
    let output = tool
        .invoke_output(
            serde_json::json!({"command": "sleep 1", "run_in_background": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let evidence = output.execution.expect("background evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Running);
    assert!(evidence.task_id.is_some());
    assert_eq!(manager.active_count(), 1);
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_bash_rejects_new_execution_after_session_owner_closes() {
    // v4-part-4 W3-C1：本条前置来自 `assembly_test.rs` 原
    // `workflow_shell_tools_reject_execution_after_session_owner_closes`——workflow 面
    // 不再持有壳工具本体（唯一提供面是 builtin `workspace` 实例的桥，其 `TaskManager`
    // 经 AW3-11 的 session 级 seam 注入），因此「owner 关闭后拒绝发起新执行」这条保证
    // 落在 `BashTool` 本层：它对链上消费面与 builtin 桥消费面逐字等价。
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().to_str().unwrap();
    let manager = Arc::new(TaskManager::new());
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    let tool = BashTool::new(cwd).with_task_manager(manager);
    let error = tool
        .invoke(
            serde_json::json!({"command": "echo leaked > unexpected"}),
            peri_agent::tools::ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("closing"), "{error}");
    assert!(!fixture.path().join("unexpected").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn test_bash_typed_evidence_marks_promoted_timeout_still_running() {
    let manager = Arc::new(TaskManager::new());
    let tool =
        BashTool::new(std::env::temp_dir().to_str().unwrap()).with_task_manager(manager.clone());
    let output = tool
        .invoke_output(
            serde_json::json!({"command": "sleep 1", "timeout": 100}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let evidence = output.execution.expect("promoted timeout evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::RunningAfterTimeout);
    assert!(evidence.task_id.is_some());
    assert!(evidence.output_truncated);
    assert_eq!(manager.active_count(), 1);
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn test_production_dispatch_persists_bash_tail_failure_evidence() {
    let fixture = tempfile::tempdir().unwrap();
    let context = dispatch_context(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()),
    );
    let outcome = dispatch_bash(
        &context,
        serde_json::json!({
            "command": output_command(20000, "TAIL_FAILURE", 7)
        }),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = &outcome.results[0].1;
    assert!(result.is_error);
    let evidence = result.execution.as_ref().expect("typed Bash evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Failed);
    assert_eq!(evidence.exit_code, Some(7));
    assert!(evidence.output_truncated);
    let output_ref = evidence.output_ref.as_ref().expect("full output ref");
    assert!(std::fs::read_to_string(output_ref)
        .unwrap()
        .contains(&format!("{}TAIL_FAILURE", "x".repeat(20_000))));
    assert!(result.output.chars().count() <= 10_000);

    let transcript = context.session.transcript.read();
    let message = transcript
        .visible_messages()
        .into_iter()
        .find(|message| matches!(message, BaseMessage::Tool { .. }))
        .cloned()
        .expect("canonical tool message");
    let encoded = serde_json::to_string(&message).unwrap();
    let restored: BaseMessage = serde_json::from_str(&encoded).unwrap();
    assert!(matches!(
        restored,
        BaseMessage::Tool {
            execution: Some(_),
            is_error: true,
            ..
        }
    ));
    maybe_export_fixture(std::slice::from_ref(&message));
}

#[tokio::test]
async fn test_production_dispatch_persists_bash_success_projection_and_ref() {
    let fixture = tempfile::tempdir().unwrap();
    let context = dispatch_context(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()),
    );
    let outcome = dispatch_bash(
        &context,
        serde_json::json!({
            "command": output_command(20000, "", 0)
        }),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = &outcome.results[0].1;
    assert!(!result.is_error);
    let evidence = result.execution.as_ref().expect("typed Bash evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Completed);
    assert_eq!(evidence.exit_code, Some(0));
    assert!(evidence.output_truncated);
    let output_ref = evidence.output_ref.as_ref().expect("full output ref");
    assert!(std::fs::read_to_string(output_ref).unwrap().len() > 10_000);
    assert!(result.output.chars().count() <= 10_000);
}

#[tokio::test]
async fn test_production_dispatch_live_tool_end_matches_canonical_projection() {
    let fixture = tempfile::tempdir().unwrap();
    let (context, mut handles) = dispatch_context_with_events(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()),
    );
    let outcome = dispatch_bash(
        &context,
        serde_json::json!({
            "command": output_command(20000, "", 7)
        }),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = &outcome.results[0].1;
    let evidence = result.execution.as_ref().expect("typed Bash evidence");
    assert_eq!(evidence.exit_code, Some(7));
    assert!(evidence.output_truncated);
    let event = loop {
        let event = handles.try_render().expect("ToolEnded render event");
        if let peri_acp_types::event_v2::RenderEvent::ToolEnded { .. } = event {
            break event;
        }
    };
    let peri_acp_types::event_v2::RenderEvent::ToolEnded {
        output, is_error, ..
    } = event
    else {
        unreachable!();
    };
    assert_eq!(output, result.output);
    assert_eq!(is_error, result.is_error);
    assert!(output.chars().count() <= 10_000);
}

#[tokio::test]
async fn test_production_dispatch_near_limit_output_persists_before_metadata_projection() {
    let fixture = tempfile::tempdir().unwrap();
    let context = dispatch_context(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()),
    );
    let outcome = dispatch_bash(
        &context,
        serde_json::json!({
            "command": output_command(9900, "", 0)
        }),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = &outcome.results[0].1;
    let evidence = result.execution.as_ref().expect("typed evidence");
    assert!(evidence.output_truncated);
    let output_ref = evidence.output_ref.as_ref().expect("full output ref");
    assert!(std::fs::read_to_string(output_ref).unwrap().len() >= 9_900);
    assert!(result.output.contains("status: completed"));
    assert!(result.output.chars().count() <= 10_000);
}

#[cfg(unix)]
#[tokio::test]
async fn test_production_dispatch_persists_promoted_timeout_lifecycle() {
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let context = dispatch_context(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()).with_task_manager(manager.clone()),
    );
    let outcome = dispatch_bash(
        &context,
        serde_json::json!({"command": "sleep 1", "timeout": 100}),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = &outcome.results[0].1;
    let evidence = result.execution.as_ref().expect("timeout evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::RunningAfterTimeout);
    assert!(evidence.task_id.is_some());
    assert!(result.is_error);
    assert!(result.output.chars().count() <= 10_000);
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_production_dispatch_persists_outer_cancel_evidence() {
    let fixture = tempfile::tempdir().unwrap();
    let context = dispatch_context(
        fixture.path(),
        BashTool::new(fixture.path().to_string_lossy()),
    );
    let cancel = CancellationToken::new();
    let task_context = context.clone();
    let task_cancel = cancel.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_bash(
            &task_context,
            serde_json::json!({"command": "sleep 2"}),
            task_cancel,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let error = match dispatch.await.unwrap() {
        Ok(_) => panic!("cancel must interrupt dispatch"),
        Err(error) => error,
    };
    assert!(matches!(error, peri_agent::error::AgentError::Interrupted));
    let transcript = context.session.transcript.read();
    let message = transcript
        .visible_messages()
        .into_iter()
        .find_map(|message| match message {
            BaseMessage::Tool {
                execution,
                is_error,
                ..
            } => Some((execution, is_error)),
            _ => None,
        })
        .expect("canonical cancelled tool message");
    let (evidence, is_error) = message;
    assert_eq!(
        evidence.as_ref().expect("cancel evidence").status,
        ToolExecutionStatus::Cancelled
    );
    assert!(is_error);
}
