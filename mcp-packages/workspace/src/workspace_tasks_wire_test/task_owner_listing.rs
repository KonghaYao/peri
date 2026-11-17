// ── 阶段 4：按直接 owner 查询存活任务（host-only `shell_task_list`）────────────

fn scope_meta(owner: &str, thread_owner: &str) -> rmcp::model::RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_OWNER_META_KEY.into(), owner.into());
    fields.insert(crate::THREAD_OWNER_META_KEY.into(), thread_owner.into());
    rmcp::model::RequestMetaObject(rmcp::model::MetaObject(fields))
}

/// 调用 host-only 查询面并解析 JSON 文本；调用本身不得是工具错误。
async fn shell_task_list(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: Option<&str>,
) -> serde_json::Value {
    let mut arguments = serde_json::Map::new();
    if let Some(owner) = owner {
        arguments.insert("owner".into(), owner.into());
    }
    let request = CallToolRequestParams::new("shell_task_list").with_arguments(arguments);
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("shell_task_list 必须可调用")
    else {
        panic!("查询必须返回 complete 结果")
    };
    assert_ne!(result.is_error, Some(true), "查询失败不得伪装成空列表");
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("查询结果必须是 JSON: {error}: {text}"))
}

/// 按 owner 过滤只返回该 owner 的存活任务；`threadOwner` 来自 `peri/threadOwner`。
#[tokio::test]
async fn shell_task_list_filters_live_tasks_by_direct_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;

    // 查询面不进模型工具清单：七工具面保持不变。
    let tools = client.peer().list_all_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 7, "host-only 查询不得进入模型工具面");

    let mut first = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    first.meta = Some(scope_meta("session-a", "thread-a"));
    let CallToolResponse::Task(first_task) = client
        .peer()
        .call_tool_once(first)
        .await
        .expect("owner A task")
    else {
        panic!("task handle")
    };

    let mut second = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    second.meta = Some(scope_meta("session-b", "thread-b"));
    let CallToolResponse::Task(second_task) = client
        .peer()
        .call_tool_once(second)
        .await
        .expect("owner B task")
    else {
        panic!("task handle")
    };

    let listed = shell_task_list(&client, Some("session-a")).await;
    assert_eq!(listed["unknown"], serde_json::json!(false));
    let tasks = listed["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "只返回该 owner 的存活任务: {listed}");
    assert_eq!(tasks[0]["taskId"], first_task.task.task_id);
    assert_eq!(tasks[0]["owner"], "session-a");
    assert_eq!(tasks[0]["threadOwner"], "thread-a");
    assert_eq!(tasks[0]["state"], "working");
    assert!(tasks[0]["startedAt"]
        .as_str()
        .is_some_and(|v| !v.is_empty()));

    let listed = shell_task_list(&client, Some("session-b")).await;
    let tasks = listed["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "owner B 只看得到自己的任务: {listed}");
    assert_eq!(tasks[0]["taskId"], second_task.task.task_id);

    let listed = shell_task_list(&client, Some("session-c")).await;
    assert_eq!(
        listed["unknown"],
        serde_json::json!(false),
        "已确认没有任务必须是 known-empty，而不是 unknown"
    );
    assert_eq!(listed["tasks"], serde_json::json!([]));

    for (owner, task) in [("session-a", &first_task), ("session-b", &second_task)] {
        let mut cancel = CancelTaskParams::new(&task.task.task_id);
        cancel.meta = Some(owner_meta(owner));
        client
            .peer()
            .cancel_task(cancel)
            .await
            .expect("cancel owned task");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let a = shell_task_list(&client, Some("session-a")).await;
            let b = shell_task_list(&client, Some("session-b")).await;
            if a["tasks"] == serde_json::json!([]) && b["tasks"] == serde_json::json!([]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal tasks leave the live list");

    drop(client);
    server_task.await.expect("join server");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

/// 没有 Shell 任务状态的实例：unknown + detail，且 `tasks` 为空；不是 MCP 错误。
#[tokio::test]
async fn shell_task_list_without_task_state_reports_unknown() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::new(dir.path().to_string_lossy(), None);
    let (client, server_task) = connect(server, ClientCapabilities::default()).await;

    let listed = shell_task_list(&client, None).await;
    assert_eq!(listed["unknown"], serde_json::json!(true));
    assert_eq!(listed["tasks"], serde_json::json!([]));
    assert!(
        listed["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not enabled")),
        "未知必须携带可定位的原因: {listed}"
    );

    drop(client);
    server_task.await.expect("join server");
}

/// 恢复协调器以 thread id 查询（设计 §6）；session id 与 thread id 都是直接 owner，
/// 等值匹配即可看到该任务，不相干 owner 仍是 known-empty。
#[tokio::test]
async fn shell_task_list_matches_session_and_thread_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;

    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(scope_meta("session-a", "thread-a"));
    let CallToolResponse::Task(task) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("owned task")
    else {
        panic!("task handle")
    };

    for owner in ["session-a", "thread-a"] {
        let listed = shell_task_list(&client, Some(owner)).await;
        assert_eq!(listed["unknown"], serde_json::json!(false));
        let tasks = listed["tasks"].as_array().expect("tasks array");
        assert_eq!(
            tasks.len(),
            1,
            "{owner} 必须看到直接归属的存活任务: {listed}"
        );
        assert_eq!(tasks[0]["taskId"], task.task.task_id);
    }

    let listed = shell_task_list(&client, Some("thread-other")).await;
    assert_eq!(listed["unknown"], serde_json::json!(false));
    assert_eq!(listed["tasks"], serde_json::json!([]));

    let mut cancel = CancelTaskParams::new(&task.task.task_id);
    cancel.meta = Some(owner_meta("session-a"));
    client
        .peer()
        .cancel_task(cancel)
        .await
        .expect("cancel owned task");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let by_thread = shell_task_list(&client, Some("thread-a")).await;
            let by_session = shell_task_list(&client, Some("session-a")).await;
            if by_thread["tasks"] == serde_json::json!([])
                && by_session["tasks"] == serde_json::json!([])
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal tasks leave the live list");

    drop(client);
    server_task.await.expect("join server");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}
