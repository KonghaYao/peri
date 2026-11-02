use super::*;
use serial_test::serial;

/// 验证语义优先级（define.rs:399-403 的逻辑：background 优先 → 走 invoke_background_fork）。
///
/// 关键断言：
/// - 调用返回值包含 "Background task"（证明走了 background 路径）
/// - bg_event_sender 接收到 SubagentStarted（is_background=true）
/// - background registry 中注册了任务（task_id 前缀为 "bg-"）
/// - 捕获的 mock LLM prompt 包含 `<fork_directive>`（英文模板，BgForkDirectiveKind::Fork）
///   而非 `<bg_fork_directive>`（中文模板）——证明 directive kind 正确
#[tokio::test]
async fn test_integration_fork_plus_background_priority() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    // Arrange: 捕获 LLM 收到的 prompt（用于验证 directive kind）
    let prompt_capture: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let prompt_capture_clone = Arc::clone(&prompt_capture);

    #[derive(Clone)]
    struct PromptCaptureLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    impl PromptCaptureLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let _tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            // 找到最后一条 Human 消息（fork directive 在 prompt queue 里）
            if let Some(last_human) = messages
                .iter()
                .rev()
                .find(|m| matches!(m, BaseMessage::Human { .. }))
            {
                *self.captured.lock().unwrap() = last_human.content();
            }
            text_events("bg-fork done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(PromptCaptureLLM);

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("ctx for bg fork"));

    let host = HostFixture::open_with_background(
        "fixture-iv2-independent",
        Arc::clone(&registry),
        bg_tx.clone(),
    )
    .await;
    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(PromptCaptureLLM {
                        captured: Arc::clone(&prompt_capture_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx),
    );

    // Act: 同时 fork=true + run_in_background=true（优先级测试）
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "do both"
            }),
            host.context(&[]),
        )
        .await
        .unwrap();

    // Assert 1: 走 background 路径（返回值包含 "Background task"）
    assert!(
        result.contains("Background task"),
        "fork+bg should prioritize background path: {}",
        result
    );

    // Assert 2: 从返回值中提取 task_id，验证前缀为 "bg-"
    // 格式: "Background task bg-{uuid} started..."
    let task_id = result
        .split("Background task ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("task_id should be parseable from result");
    assert!(
        task_id.starts_with("bg-"),
        "task_id should have 'bg-' prefix (background spawn), got: {}",
        task_id
    );

    // Assert 3: registry 中注册了任务（active_count >= 1）
    assert!(
        registry.active_count() >= 1,
        "background fork should be registered in BackgroundTaskRegistry"
    );

    // Assert 4: bg_event_sender 收到 SubagentStarted（is_background=true）
    let started_ev = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("should receive SubagentStarted within timeout")
        .expect("channel should not be closed");
    match started_ev.payload().clone() {
        ExecutorEvent::SubagentStarted { is_background, .. } => {
            assert!(
                is_background,
                "SubagentStarted should have is_background=true for fork+bg"
            );
        }
        other => panic!("expected SubagentStarted first, got: {:?}", other),
    }

    // Assert 5: 等待 background task 完成，捕获 LLM 收到的 prompt
    // 验证 directive kind = Fork（英文 `<fork_directive>`，非中文 `<bg_fork_directive>`）
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bg_rx.recv().await {
                Some(ExecutorEvent::BackgroundTaskCompleted(_)) => break,
                Some(_) => continue,
                None => break,
            }
        }
    })
    .await;

    let captured_prompt = prompt_capture.lock().unwrap().clone();
    assert!(
        captured_prompt.contains("<fork_directive>"),
        "fork+bg should use Fork directive kind (English <fork_directive>), got: {}",
        captured_prompt
    );
    assert!(
        !captured_prompt.contains("<bg_fork_directive>"),
        "fork+bg should NOT use Bg directive kind (Chinese <bg_fork_directive>), got: {}",
        captured_prompt
    );
    assert!(
        captured_prompt.contains("do both"),
        "fork directive should wrap original prompt 'do both'"
    );
}

/// [M01 回归] 命名（非 fork）同步调用不得继承父消息：父历史只应出现在 fork 路径。
#[tokio::test]
#[serial]
async fn test_named_sync_does_not_inherit_parent_messages() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("named-sync.md"),
        "---\nname: named-sync\ndescription: named sync agent\n---\n\nNamed sync.\n",
    )
    .unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-named-sync").await;
    let model = super::mock_model::RecordingModel::new("named sync done");
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
        }),
        dir.path().to_string_lossy().into_owned(),
    );
    let tool = host.bind(with_agent_face(tool, dir.path()).await);

    // 父侧 ToolContext 携带大父历史；命名子 agent 只应看到自己的 prompt。
    let parent_history = vec![
        BaseMessage::human("PARENT-HISTORY-MARKER-Q1"),
        BaseMessage::ai("PARENT-HISTORY-MARKER-A1"),
        BaseMessage::human("PARENT-HISTORY-MARKER-Q2"),
    ];
    super::super::invocation::reset_current_messages_calls();
    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "named-sync",
                "prompt": "named child task",
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&parent_history),
        )
        .await
        .expect("named sync should run");
    assert!(result.contains("named sync done"), "{result}");
    assert_eq!(
        super::super::invocation::current_messages_calls(),
        0,
        "命名同步路径不得物化父历史"
    );

    let texts = super::mock_model::conversation_texts(&model.last_messages());
    assert!(
        texts
            .iter()
            .all(|text| !text.contains("PARENT-HISTORY-MARKER")),
        "命名子 agent 不得继承父历史: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("named child task")),
        "命名子 agent 必须收到自己的 prompt: {texts:?}"
    );
}

/// [M01 回归] 命名后台调用同样不得继承父消息。
#[tokio::test]
#[serial]
async fn test_named_background_does_not_inherit_parent_messages() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("named-bg.md"),
        "---\nname: named-bg\ndescription: named background agent\n---\n\nNamed bg.\n",
    )
    .unwrap();
    let (bg_tx, mut bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_agent::agent::events::ExecutorEvent>();
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let host = HostFixture::open_in_with_background(
        dir.path(),
        "fixture-named-bg",
        Arc::clone(&manager),
        bg_tx,
    )
    .await;
    let model = super::mock_model::RecordingModel::new("named bg done");
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
        }),
        dir.path().to_string_lossy().into_owned(),
    );
    let tool = host.bind(with_agent_face(tool, dir.path()).await);

    let parent_history = vec![
        BaseMessage::human("PARENT-HISTORY-MARKER-BG-Q1"),
        BaseMessage::ai("PARENT-HISTORY-MARKER-BG-A1"),
    ];
    super::super::invocation::reset_current_messages_calls();
    let started = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "named-bg",
                "prompt": "named background task",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&parent_history),
        )
        .await
        .expect("named background should start");
    assert!(started.contains("Background task"), "{started}");

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = bg_rx.recv().await {
            if matches!(
                event,
                peri_agent::agent::events::ExecutorEvent::BackgroundTaskCompleted(_)
            ) {
                break;
            }
        }
    })
    .await
    .expect("named background must complete within timeout");
    assert_eq!(
        super::super::invocation::current_messages_calls(),
        0,
        "命名后台路径不得物化父历史"
    );

    let texts = super::mock_model::conversation_texts(&model.last_messages());
    assert!(!texts.is_empty(), "后台命名子 agent 必须实际发起模型请求");
    assert!(
        texts
            .iter()
            .all(|text| !text.contains("PARENT-HISTORY-MARKER")),
        "命名后台子 agent 不得继承父历史: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("named background task")),
        "命名后台子 agent 必须收到自己的 prompt: {texts:?}"
    );
    peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;
}
