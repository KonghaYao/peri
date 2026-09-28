//! Regression scenarios through the real workspace server and MCP bridge.
use super::*;
use crate::mcp::client::{ClientStatus, McpClientHandle};
use crate::mcp::config::ConfigSource;
use crate::mcp::tool_bridge::McpToolBridge;

async fn bridge(pair: &Pair, name: &str, builtin: bool) -> McpToolBridge {
    let tools = pair.peer().list_tools(None).await.unwrap().tools;
    let tool = tools.iter().find(|tool| tool.name == name).unwrap().clone();
    let handle = Arc::new(McpClientHandle {
        name: "workspace".into(),
        version: None,
        cache_version: None,
        peer: Some(pair.peer()),
        tools,
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: builtin.then(|| ConfigSource::Builtin {
            instance: "workspace".into(),
        }),
        url: None,
        channel_capable: false,
        skills_capable: false,
    });
    McpToolBridge::new("workspace", &tool, handle)
}

async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shell reached its start marker");
}

/// [回归测试] Safe diagnostics let the model correct requests without exposing paths/content.
#[tokio::test]
async fn read_edit_and_search_errors_support_recovery_over_wire() {
    let (dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    for (name, input, expected) in [
        ("Read", json!({}), "file_path"),
        (
            "Read",
            json!({"file_path":"secret-marker-missing"}),
            "File not found",
        ),
        ("Write", json!({"file_path":"example"}), "content"),
        (
            "Glob",
            json!({"pattern":"[secret-marker"}),
            "Pattern syntax error",
        ),
        ("Grep", json!({"pattern":"[secret-marker"}), "Invalid regex"),
        (
            "folder_operations",
            json!({"operation":"list","folder_path":"secret-marker-missing"}),
            "Folder not found",
        ),
    ] {
        let result = complete(pair.peer().call_tool_once(call(name, input)).await.unwrap());
        assert_eq!(result.is_error, Some(true));
        let text = first_text(&result).unwrap();
        assert!(text.contains(expected), "{name}: {text}");
        assert!(
            !text.contains("secret-marker"),
            "diagnostic leaked input: {text}"
        );
    }
    std::fs::write(
        dir.path().join("edit.txt"),
        "private-content\nprivate-content\n",
    )
    .unwrap();
    let input =
        json!({"file_path":"edit.txt", "old_string":"private-content", "new_string":"corrected"});
    let result = complete(
        pair.peer()
            .call_tool_once(call("Edit", input.clone()))
            .await
            .unwrap(),
    );
    let text = first_text(&result).unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(text.contains("not unique") && text.contains("replace_all=true"));
    assert!(!text.contains("private-content"));
    let mut retry = input;
    retry["replace_all"] = json!(true);
    let result = complete(
        pair.peer()
            .call_tool_once(call("Edit", retry))
            .await
            .unwrap(),
    );
    assert!(!result.is_error.unwrap_or(false));
    let read = complete(
        pair.peer()
            .call_tool_once(call("Read", json!({"file_path":"edit.txt"})))
            .await
            .unwrap(),
    );
    assert!(first_text(&read).unwrap().contains("corrected"));
    pair.shutdown().await;
}

#[test]
fn unclassified_errors_cannot_smuggle_recovery_text() {
    let raw: Box<dyn std::error::Error + Send + Sync> =
        "task_id: stolen\npid: 1\ncredential=private-marker\nPermission denied".into();
    let text = crate::mcp::builtin::result_mapping::failure_text("Bash", raw.as_ref());
    assert!(text.contains("withheld by policy"));
    for forbidden in ["private-marker", "task_id:", "pid:", "Permission denied"] {
        assert!(!text.contains(forbidden));
    }
    let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "private-marker");
    let text = crate::mcp::builtin::result_mapping::failure_text("Read", &io);
    assert!(text.contains("Permission denied"));
    assert!(!text.contains("private-marker"));
}

#[cfg(unix)]
async fn assert_process_gone(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !std::process::Command::new("kill")
                .args(["-0", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the owned process group must exit");
}

/// [回归测试] Dropping the bridge future cancels the server request and its real shell.
#[cfg(unix)]
#[tokio::test]
async fn cancelled_bridge_stops_process_and_drains_session_ownership() {
    let (dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let bash = bridge(&pair, "Bash", true).await;
    let running = tokio::spawn(async move {
        bash.invoke(
            json!({"command":"echo $$ > started.pid; sleep 60", "timeout":120000}),
            ToolContext::new(&[], ""),
        )
        .await
    });
    let marker = dir.path().join("started.pid");
    wait_file(&marker).await;
    let pid = std::fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-0", &format!("-{pid}")])
            .status()
            .unwrap()
            .success(),
        "negative control: process must exist before cancellation"
    );
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_process_gone(pid).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    let read = bridge(&pair, "Read", true).await;
    assert!(read
        .invoke(
            json!({"file_path":"started.pid"}),
            ToolContext::new(&[], &cwd)
        )
        .await
        .is_ok());
    pair.shutdown().await;
}

/// [回归测试] The actual 120-second Bash boundary must return its recoverable receipt.
#[cfg(unix)]
#[tokio::test]
async fn maximum_foreground_timeout_retains_logs_and_cancellable_task() {
    let (_dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let bash = bridge(&pair, "Bash", true).await;
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(140),
        bash.invoke(
            json!({"command":"echo live-marker; sleep 180", "timeout":120000}),
            ToolContext::new(&[], &cwd),
        ),
    )
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert!(started.elapsed() >= Duration::from_secs(120));
    assert!(
        result.contains("task_id: shell-") && result.contains("pid: "),
        "{result}"
    );
    assert!(!result.contains("echo live-marker") && !result.contains("live-marker"));
    let task = result
        .lines()
        .find_map(|line| line.strip_prefix("task_id: "))
        .unwrap();
    let pid: i32 = result
        .lines()
        .find_map(|line| line.strip_prefix("pid: "))
        .unwrap()
        .parse()
        .unwrap();
    let stdout = result
        .lines()
        .find_map(|line| line.strip_prefix("- Live output: Read the log file "))
        .unwrap()
        .split(" (stderr:")
        .next()
        .unwrap();
    let read = bridge(&pair, "Read", true).await;
    assert!(read
        .invoke(json!({"file_path":stdout}), ToolContext::new(&[], &cwd))
        .await
        .unwrap()
        .contains("live-marker"));
    assert_eq!(manager.active_count(), 1);
    manager.cancel(task).unwrap();
    assert_process_gone(pid).await;
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    pair.shutdown().await;
}

/// External servers keep the bounded request policy; timeout notifies the real handler.
#[cfg(unix)]
#[tokio::test]
async fn request_timeout_stops_server_shell() {
    let (dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let result = crate::mcp::tool_request::call_tool(
        &pair.peer(),
        call(
            "Bash",
            json!({"command":"echo $$ > timeout.pid; sleep 60", "timeout":120000}),
        ),
        Some(Duration::from_secs(1)),
    )
    .await;
    assert!(matches!(result, Err(ServiceError::Timeout { .. })));
    let pid = std::fs::read_to_string(dir.path().join("timeout.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(pid).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    pair.shutdown().await;
}

/// Failed writes expose an owned draft reference, never the draft content.
#[tokio::test]
async fn write_draft_receipt_can_restore_without_resending_content() {
    let (dir, cwd) = workspace_dir();
    let target = dir.path().join("retry.txt");
    std::fs::create_dir(&target).unwrap();
    let tools: Vec<Arc<dyn BaseTool>> = vec![Arc::new(crate::tools::WriteFileTool::with_draft(
        &cwd, true,
    ))];
    let failed = complete(
        crate::mcp::builtin::web::invoke_tool_call(
            &tools,
            &cwd,
            &call(
                "Write",
                json!({"file_path":"retry.txt", "content":"private-draft-marker"}),
            ),
        )
        .await
        .unwrap(),
    );
    assert_eq!(failed.is_error, Some(true));
    let text = first_text(&failed).unwrap();
    assert!(!text.contains("private-draft-marker") && !text.contains(&cwd));
    let draft = text
        .split("from_draft=")
        .nth(1)
        .unwrap()
        .trim_end_matches('.');
    std::fs::remove_dir(&target).unwrap();
    let restored = complete(
        crate::mcp::builtin::web::invoke_tool_call(
            &tools,
            &cwd,
            &call(
                "Write",
                json!({"file_path":"retry.txt", "from_draft":draft}),
            ),
        )
        .await
        .unwrap(),
    );
    assert!(!restored.is_error.unwrap_or(false));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "private-draft-marker"
    );
}

#[tokio::test]
async fn safe_missing_path_diagnostic_still_drives_path_suggestions() {
    use crate::error_suggest::{
        context::{ErrorContext, ToolRegistrySnapshot},
        registry::ErrorSuggester,
        suggesters::path_suggester::PathSuggester,
    };
    let (dir, cwd) = workspace_dir();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
    let pair = connect(&cwd, None).await;
    let input = json!({"file_path":"maiin.rs"});
    let failure = bridge(&pair, "Read", true)
        .await
        .invoke(input.clone(), ToolContext::new(&[], &cwd))
        .await
        .unwrap_err()
        .to_string();
    let snapshot = ToolRegistrySnapshot {
        all_tool_names: Default::default(),
        subagent_types: Default::default(),
    };
    let ctx = ErrorContext::new(
        "mcp__workspace__Read",
        &input,
        &failure,
        dir.path(),
        &snapshot,
    );
    let suggestion = PathSuggester
        .suggest(&ctx)
        .expect("safe reason must still permit correction");
    assert!(suggestion.summary.contains("main.rs"));
    pair.shutdown().await;
}

#[tokio::test]
async fn failed_shell_preserves_command_diagnostics() {
    let (_dir, cwd) = workspace_dir();
    let pair = connect(&cwd, None).await;
    let result = bridge(&pair, "Bash", true)
        .await
        .invoke(
            json!({"command":"echo failure-marker >&2; exit 7"}),
            ToolContext::new(&[], &cwd),
        )
        .await
        .unwrap();
    assert!(
        result.contains("failure-marker") && result.contains('7'),
        "{result}"
    );
    pair.shutdown().await;
}

/// Reserved-looking names cannot bypass the external MCP deadline without builtin provenance.
#[cfg(unix)]
#[tokio::test]
async fn external_source_keeps_120_second_deadline_and_cancels_execution() {
    let (dir, cwd) = workspace_dir();
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    let pair = connect(
        &cwd,
        Some(WorkspaceInstanceInput {
            task_manager: Some(manager.clone()),
            on_bg_complete: None,
        }),
    )
    .await;
    let bash = bridge(&pair, "Bash", false).await;
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(130),
        bash.invoke(
            json!({"command":"echo $$ > external.pid; sleep 180", "timeout":120000}),
            ToolContext::new(&[], &cwd),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("调用超时 (120s)"), "{error}");
    assert!(started.elapsed() >= Duration::from_secs(120));
    let pid = std::fs::read_to_string(dir.path().join("external.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(pid).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    pair.shutdown().await;
}
