use super::*;

// ─── Slice 6:Agent 工具 resume_thread_id 参数（tool 层） ──────────────────────

/// 回归（占位符劫持）：LLM 表达「省略/意图」时会把 resume_thread_id 填成
/// "" / "new" / "__omit__" 等非 UUID 占位符——必须忽略并走新建路径，
/// 而不是进入 resume 分支报 invalid thread id（曾导致 subagent 高失败率死循环）。
#[tokio::test]
async fn test_resume_thread_id_placeholder_ignored_and_spawns_new() {
    for placeholder in ["", "new", "__omit__"] {
        let dir = tempdir().unwrap();
        write_test_agent(&dir);
        let _fixture = SessionFixture::open_in(dir.path()).await;
        let host = HostFixture::open_in(dir.path(), "fixture-resume-placeholder").await;
        let t = host.bind(with_agent_face(make_subagent_tool(vec![]), dir.path()).await);
        let result = t
            .invoke(
                serde_json::json!({
                    "resume_thread_id": placeholder,
                    "subagent_type": "test-agent",
                    "cwd": host.cwd.clone(),
                    "prompt": "do it",
                }),
                host.context(&[]),
            )
            .await;
        assert!(
            result.is_ok(),
            "占位符 resume_thread_id {:?} 应被忽略并走新建路径: {:?}",
            placeholder,
            result.err()
        );
        let result = result.unwrap();
        assert!(
            result.contains("child_thread_id:"),
            "新建路径返回值应带 child_thread_id: {}",
            result
        );
        assert!(
            !result.contains("invalid thread id"),
            "不应触发 invalid thread id: {}",
            result
        );
    }
}

/// R-M2 容错：resume_thread_id 与 fork 同传 → fork 被忽略，恢复成功（不报错）
#[tokio::test]
async fn test_resume_thread_id_ignores_fork_field() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        Some(parent_id.as_str()),
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = with_agent_face(make_subagent_tool(vec![]), dir.path())
        .await
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "fork": true,
                "cwd": cwd.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect("resume+fork 应容错恢复而非报互斥错误");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
}

/// R-M2 容错：resume_thread_id 与 subagent_type 同传 → subagent_type 被忽略，
/// 恢复成功（不报错）
#[tokio::test]
async fn test_resume_thread_id_ignores_subagent_type_field() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        Some(parent_id.as_str()),
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = with_agent_face(make_subagent_tool(vec![]), dir.path())
        .await
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "subagent_type": "test-agent",
                "cwd": cwd.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect("resume+subagent_type 应容错恢复而非报互斥错误");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
}

/// 校验：thread 不存在 → Err（thread not found，agent 层统一前缀）
#[tokio::test]
async fn test_resume_thread_id_not_found() {
    let dir = tempdir().unwrap();
    let fixture = SessionFixture::open_in(dir.path()).await;
    let cwd = fixture.workspace_cwd();
    let parent_id = fixture
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let t = make_subagent_tool(vec![])
        .with_session_resources(fixture.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": uuid::Uuid::now_v7().to_string(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let error = result.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<peri_agent::tools::EffectiveToolError>()
            .expect("missing target must be a typed preflight rejection")
            .code,
        peri_agent::tools::EffectiveToolErrorCode::InvalidInput,
    );
    let err = error.to_string();
    assert!(
        err.contains("thread not found"),
        "不存在的 thread 应报 not found: {}",
        err
    );
    assert!(fixture.load_meta(&parent_id).await.is_ok());
    assert!(fixture
        .facade()
        .list_children(&parent_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn test_resume_missing_resources_is_a_typed_preflight_rejection() {
    let tool = make_subagent_tool(vec![]);
    let error = tool
        .invoke_resume(
            uuid::Uuid::now_v7().to_string(),
            Some("ping".into()),
            "/tmp".into(),
            false,
            None,
        )
        .await
        .unwrap_err();
    let rejection = error
        .downcast_ref::<peri_agent::tools::EffectiveToolError>()
        .unwrap();
    assert_eq!(
        rejection.code,
        peri_agent::tools::EffectiveToolErrorCode::ApplicationFailed
    );
    assert!(rejection.message.contains("session resources required"));
}

#[tokio::test]
async fn test_resume_invalid_identity_is_a_typed_preflight_rejection() {
    let dir = tempdir().unwrap();
    let fixture = SessionFixture::open_in(dir.path()).await;
    let tool = make_subagent_tool(vec![]).with_session_resources(fixture.facade());
    let error = tool
        .invoke_resume(
            "../invalid".into(),
            Some("ping".into()),
            fixture.workspace_cwd(),
            false,
            None,
        )
        .await
        .unwrap_err();
    let rejection = error
        .downcast_ref::<peri_agent::tools::EffectiveToolError>()
        .unwrap();
    assert_eq!(
        rejection.code,
        peri_agent::tools::EffectiveToolErrorCode::InvalidInput
    );
    assert!(rejection.message.contains("invalid thread id"));
}

/// 校验：thread 状态 active（未正常收尾）→ Err（R-M4 文本）。
/// title 用 "fork"——fork 路径不依赖 agent_def，可先于 resume 校验触达
#[tokio::test]
async fn test_resume_thread_id_active_rejected() {
    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = id.clone();
    meta.title = Some("fork".to_string());
    store.create_thread(meta).await.unwrap(); // ThreadMeta 默认 agent_status = Active
    let t = make_subagent_tool(vec![])
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id,
            }),
            preset_child_ctx(&id, "."),
        )
        .await;
    let error = result.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<peri_agent::tools::EffectiveToolError>()
            .expect("active target without a receiver must be a typed rejection")
            .code,
        peri_agent::tools::EffectiveToolErrorCode::ApplicationFailed,
    );
    let err = error.to_string();
    assert!(
        err.contains("is still active"),
        "active thread 应被拒绝: {}",
        err
    );
}

/// [回归测试] 同工作区绑定不能越过执行根归属；拒绝不得启动 child 或修改历史。
///
/// 历史背景：旧断言绑定整句错误文案，未区分执行根归属与 SDK 的执行唯一性。
/// 显式 resume 只加载历史并开始新 run，不恢复旧执行，也不因持有 child ID 获权。
#[tokio::test]
async fn test_resume_thread_id_parent_mismatch_is_rejected_by_root_ownership() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    // 另一个真实根会话：child 挂在它下面，祖先链可解析但执行根不同。
    let other_root = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立另一根会话失败");
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        Some(other_root.as_str()),
        vec![BaseMessage::human("旧消息")],
    )
    .await;

    // 调用方自己的父会话（最后一个建，夹具执行所有权就是它的）
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let history_before = store.resources.load_session_history(&id).await.unwrap();
    let model = mock_model::RecordingModel::new("must not run across roots");
    let child_model = Arc::clone(&model);
    let t = with_agent_face(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_| SubagentLlmSource::model(child_model.clone(), "root-ownership")),
            cwd.clone(),
        ),
        dir.path(),
    )
    .await
    .with_session_resources(store.facade())
    .with_parent_thread_id(parent_id.clone())
    .with_parent_session(parent);
    let error = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect_err("跨根恢复必须被拒绝");
    assert!(
        error
            .to_string()
            .contains("belongs to another root session"),
        "拒绝原因应为执行根归属，而不是「不存在」或「仍处于运行态」: {error}"
    );
    assert_eq!(model.call_count(), 0, "跨根拒绝不得启动新的 child run");
    assert_eq!(
        store
            .resources
            .load_session_history(&id)
            .await
            .unwrap()
            .iter()
            .map(|payload| peri_acp_types::store::serialize_persisted_payload(payload).unwrap())
            .collect::<Vec<_>>(),
        history_before
            .iter()
            .map(|payload| peri_acp_types::store::serialize_persisted_payload(payload).unwrap())
            .collect::<Vec<_>>(),
        "跨根拒绝不得写入 continue 或重放历史工具调用"
    );
    // 拒绝发生在任何写入之前：thread 保持原收尾状态，不留 active 残留。
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(
        meta.agent_status,
        peri_agent::thread::AgentStatus::Done,
        "被拒绝的恢复不得改动 thread 状态"
    );
}

/// [回归测试] 后台 resume 的新任务必须在直接父 host 结算并投递完成结果。
///
/// 历史背景：给工具 fallback 配置通道不会覆盖已经装配的父 host。
#[tokio::test]
async fn test_resume_thread_id_background_combination() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "fork", Some(parent_id.as_str()), Vec::new()).await;

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    parent.set_subagent_host(peri_agent::session::subagent::SubagentHost {
        session_resources: Some(store.facade()),
        parent_thread_id: Some(parent_id.clone()),
        task_manager: Some(Arc::clone(&registry)),
        bg_event_sender: Some(bg_tx),
        on_bg_complete: Some(peri_agent::session::bg_complete::task_bg_complete_callback(
            peri_agent::session::bg_complete::queue_terminal_delivery(parent.queue().clone()),
        )),
        ..Default::default()
    });
    let t = make_subagent_tool(vec![])
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "run_in_background": true,
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect("resume+bg 应启动后台任务");
    assert!(
        result.contains("Background task"),
        "bg resume 应返回启动确认文本: {}",
        result
    );
    assert!(
        result.contains("bg-"),
        "bg 启动文本应携带 task_id（bg- 前缀）: {}",
        result
    );
    assert!(
        result.contains(&id),
        "bg 启动文本应携带 thread_id: {}",
        result
    );

    // BackgroundTaskResult.child_thread_id = 恢复的 thread_id（bg 通知可再次恢复）
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bg_rx.recv().await {
                Some(ExecutorEvent::BackgroundTaskCompleted(res)) => return res,
                Some(_) => continue,
                None => panic!("bg 通道关闭"),
            }
        }
    })
    .await
    .expect("bg resume 应在超时内完成");
    assert!(completed.success);
    assert!(result.contains(&completed.task_id));
    assert!(completed
        .output
        .contains("Continue your previous task where you left off."));
    assert_eq!(
        completed.child_thread_id.as_deref(),
        Some(id.as_str()),
        "BackgroundTaskResult 必须携带 child_thread_id"
    );
    assert_eq!(registry.active_count(), 0, "父任务目录必须已结算新任务");
    let delivered = parent.queue().drain_all();
    assert_eq!(delivered.len(), 1, "完成结果必须投递给直接发起的父会话");
    assert_eq!(
        delivered[0].source,
        peri_agent::session::MessageSource::SubAgentComplete
    );
    assert!(delivered[0].delivery_id.is_some());
    let peri_agent::session::QueuedPayload::SystemReminder(reminder) = &delivered[0].payload else {
        panic!("terminal delivery must be a trusted system reminder");
    };
    assert_eq!(
        reminder.as_reminder().metadata["task_id"],
        completed.task_id
    );
    assert_eq!(reminder.as_reminder().metadata["child_thread_id"], id);
    assert_eq!(reminder.as_reminder().metadata["success"], true);
    assert!(reminder.as_reminder().body.contains(&completed.output));
    assert_eq!(
        store.load_meta(&id).await.unwrap().agent_status,
        AgentStatus::Done
    );
}

/// 成功路径：预置非 active thread（带消息）→ resume → 完成文本含
/// child_thread_id + 结果（旧 transcript 重放；prompt 缺省 → 隐式 continue）
#[tokio::test]
async fn test_resume_thread_id_success_replays_and_completes() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        Some(parent_id.as_str()),
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = with_agent_face(make_subagent_tool(vec![]), dir.path())
        .await
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect("resume 应成功");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    // EchoLLM 回显隐式 continue 注入后的最后一条消息（prompt 缺省路径）
    assert!(result.contains("echo"), "完成文本应含执行结果: {}", result);
}

/// [回归测试] fork resume 继承父工具，并因耗尽新 run 的语义预算而失败。
///
/// 历史背景：旧夹具固定调用 ID 并持续请求不存在工具，基线提前报执行错误而非耗尽预算。
/// 每轮改为有效工具及唯一调用身份，并核对 typed 失败原因，避免只按模型调用数猜测预算。
#[tokio::test]
async fn test_resume_thread_id_fork_title_uses_parent_tools_and_default_iterations() {
    let dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "fork",
        Some(parent_id.as_str()),
        vec![BaseMessage::human("task")],
    )
    .await;

    // 计数 + 工具捕获 LLM：每轮调用继承的工具 → 循环持续到迭代上限
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&llm_calls);
    let tools_clone = Arc::clone(&tools_capture);
    #[derive(Clone)]
    struct ForkLoopLLM {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl ForkLoopLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let _messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            let call_index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
                format!("fork-resume-{call_index}"),
                "Read",
                serde_json::json!({}),
            )])
        }
    }
    crate::subagent::test_support::fixture_model_impl!(ForkLoopLLM);

    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let t = SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(ForkLoopLLM {
                    calls: Arc::clone(&calls_clone),
                    captured: Arc::clone(&tools_clone),
                }),
                "fixture-scripted",
            )
        }),
        "/tmp".to_string(),
    )
    .with_session_resources(store.facade())
    .with_parent_thread_id(parent_id.clone())
    .with_parent_session(parent.clone());

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await;
    // 迭代上限耗尽 → MaxIterationsExceeded 错误（fork resume 上限 = DEFAULT_SUBAGENT_MAX_ITERATIONS）
    let error = result.unwrap_err();
    let failure = error
        .downcast_ref::<peri_agent::session::subagent::SubagentFailure>()
        .expect("fork resume must fail at the execution budget, not preflight or tool dispatch");
    assert_eq!(failure.child_thread_id(), id);
    let peri_agent::error::AgentError::MaxIterationsExceeded(budget) = failure.error() else {
        panic!("expected iteration budget exhaustion, got: {failure:?}");
    };
    let err = error.to_string();
    assert!(
        err.contains("child_thread_id") && err.contains("execution failed"),
        "错误文本应带 child_thread_id 前缀（可恢复）: {}",
        err
    );
    assert_eq!(
        llm_calls.load(std::sync::atomic::Ordering::SeqCst),
        *budget,
        "fork resume 必须耗尽配置预算，不能因无效工具调用提前退出"
    );
    assert_eq!(
        *budget,
        crate::subagent::DEFAULT_SUBAGENT_MAX_ITERATIONS,
        "fork resume 的当前默认预算必须与 fork 一致"
    );
    let captured = tools_capture.lock().unwrap();
    assert!(
        captured.contains(&"Agent".to_string()),
        "fork resume 应继承父工具集（无过滤，含 Agent）: {:?}",
        *captured
    );
}

/// agent-def resume：title == agent_id → load_agent_def 重新应用过滤
/// （tools 白名单 + Agent 恒排除；与 fork resume 的"父工具集无过滤"区分；
/// build_result 的 skill_names / system_prompt 被 resume_config_base 丢弃——
/// R-H1 / F4，不重复注入）
#[tokio::test]
async fn test_resume_thread_id_agent_def_refilters_tools() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("resume-agent.md"),
        "---\nname: resume-agent\ndescription: Resume filter test\ntools:\n  - Read\n---\n\nYou are resumable.\n",
    )
    .unwrap();

    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "resume-agent",
        Some(parent_id.as_str()),
        Vec::new(),
    )
    .await;

    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools_capture_clone = Arc::clone(&tools_capture);
    #[derive(Clone)]
    struct ResumeFilterLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl ResumeFilterLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let _messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            text_events("resume-filter-done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(ResumeFilterLLM);

    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Agent")];
    let t = SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(ResumeFilterLLM {
                    captured: Arc::clone(&tools_capture_clone),
                }),
                "fixture-scripted",
            )
        }),
        "/tmp".to_string(),
    )
    .with_session_resources(store.facade())
    .with_parent_thread_id(parent_id.clone())
    .with_parent_session(parent.clone());
    let t = with_agent_face(t, dir.path()).await;

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd.clone(),
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .expect("agent-def resume 应成功");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    assert!(
        result.contains("resume-filter-done"),
        "agent-def resume 应执行完成: {}",
        result
    );
    let captured = tools_capture.lock().unwrap();
    assert_eq!(
        captured.as_slice(),
        &["Read"],
        "agent-def resume 必须按 tools 白名单重新过滤（含 Agent 排除）: {:?}",
        *captured
    );
}

/// UUID 两侧空格和多余分支字段不能改变恢复优先级；非字符串 prompt 视为缺省。
#[tokio::test]
async fn test_resume_trimmed_id_wins_over_mcp_fork_and_invalid_model() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "test-agent", Some(parent_id.as_str()), vec![]).await;
    let tool = with_agent_face(make_subagent_tool(vec![]), dir.path())
        .await
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = tool
        .invoke(
            serde_json::json!({
                "resume_thread_id": format!("  {id}\n"),
                "subagent_type": "mcp__missing__agent",
                "fork": true,
                "model": "invalid-model",
                "prompt": null,
                "cwd": cwd.clone()
            }),
            preset_child_ctx(&id, "."),
        )
        .await
        .unwrap();
    assert!(result.starts_with(&format!("child_thread_id: {id}\n")));
    assert!(result.contains("Continue your previous task where you left off."));
    // 恢复线程为 hidden；按真实根 ID 查询包含隐藏线程的会话树。
    let session_threads = store.list_session_threads(&id).await.unwrap();
    assert_eq!(session_threads.len(), 1, "恢复不得 fork 子线程");
    assert_eq!(session_threads[0].id, id);
    assert_eq!(
        store.load_meta(&id).await.unwrap().agent_status,
        peri_agent::thread::AgentStatus::Done
    );
}
