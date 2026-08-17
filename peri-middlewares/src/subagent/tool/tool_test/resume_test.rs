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
        let t = make_subagent_tool(vec![]).with_thread_store(make_fs_store(&dir));
        let result = t
            .invoke(
                serde_json::json!({
                    "resume_thread_id": placeholder,
                    "subagent_type": "test-agent",
                    "cwd": dir.path().to_str().unwrap(),
                    "prompt": "do it",
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
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
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        None,
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = make_subagent_tool(vec![]).with_thread_store(store);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "fork": true,
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        None,
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = make_subagent_tool(vec![]).with_thread_store(store);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    let t = make_subagent_tool(vec![]).with_thread_store(make_fs_store(&dir));
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": uuid::Uuid::now_v7().to_string(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("thread not found"),
        "不存在的 thread 应报 not found: {}",
        err
    );
}

/// 校验：thread 状态 active（未正常收尾）→ Err（R-M4 文本）。
/// title 用 "fork"——fork 路径不依赖 agent_def，可先于 resume 校验触达
#[tokio::test]
async fn test_resume_thread_id_active_rejected() {
    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new("/tmp");
    meta.id = id.clone();
    meta.title = Some("fork".to_string());
    store.create_thread(meta).await.unwrap(); // ThreadMeta 默认 agent_status = Active
    let t = make_subagent_tool(vec![]).with_thread_store(store);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("is still active"),
        "active thread 应被拒绝: {}",
        err
    );
}

/// parent 链不匹配不再拒绝（parent 链校验已移除）——meta.parent_thread_id 与
/// 父 session thread_id 不一致时恢复仍成功（thread_id 即恢复凭证）
#[tokio::test]
async fn test_resume_thread_id_parent_mismatch_not_rejected() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        Some("some-other-parent"),
        vec![BaseMessage::human("旧消息")],
    )
    .await;

    // 父 session：store().thread_id = "parent-uuid" ≠ meta.parent_thread_id
    let work = dir.path().to_str().unwrap();
    let parent = peri_agent::session::Session::new(
        Arc::from(work),
        peri_agent::session::FrozenContext::builder().build(),
        Some("parent-uuid".into()),
    );
    let t = make_subagent_tool(vec![])
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_parent_session(parent);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": work,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("parent 链不匹配不再拒绝恢复");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    // 与 agent 层测试对齐：锁定完成收尾（区别于 bg 启动 / interrupted 文本也带前缀）
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(
        meta.agent_status,
        peri_agent::thread::AgentStatus::Done,
        "恢复完成后收尾 done"
    );
}

/// 组合：resume + run_in_background → bg 启动确认文本（task_id + thread_id）+
/// 完成通知 BackgroundTaskResult 携带 child_thread_id（issue 决策 8 + 验收）
#[tokio::test]
async fn test_resume_thread_id_background_combination() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "fork", None, Vec::new()).await;

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let t = make_subagent_tool(vec![])
        .with_thread_store(store)
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "run_in_background": true,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    assert_eq!(
        completed.child_thread_id.as_deref(),
        Some(id.as_str()),
        "BackgroundTaskResult 必须携带 child_thread_id"
    );
}

/// 成功路径：预置非 active thread（带消息）→ resume → 完成文本含
/// child_thread_id + 结果（旧 transcript 重放；prompt 缺省 → 隐式 continue）
#[tokio::test]
async fn test_resume_thread_id_success_replays_and_completes() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        None,
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let t = make_subagent_tool(vec![]).with_thread_store(store);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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

/// fork resume：title == "fork" → 父工具集 clone（无过滤，含 Agent）+
/// 200 迭代上限（与 execute_fork.rs:48 一致）——循环 LLM 恰好耗尽 200 次
/// 后返回 MaxIterationsExceeded 错误（错误文本带 child_thread_id 前缀，可恢复）
#[tokio::test]
async fn test_resume_thread_id_fork_title_uses_parent_tools_and_200_iterations() {
    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "fork", None, vec![BaseMessage::human("task")]).await;

    // 计数 + 工具捕获 LLM：恒请求调用不存在工具 → 循环持续到迭代上限
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&llm_calls);
    let tools_clone = Arc::clone(&tools_capture);
    struct ForkLoopLLM {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ForkLoopLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            Ok(Reasoning::with_tools(
                "keep looping",
                vec![peri_agent::agent::react::ToolCall::new(
                    "id1",
                    "nonexistent",
                    serde_json::json!({}),
                )],
            ))
        }
    }

    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let t = SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ForkLoopLLM {
                calls: Arc::clone(&calls_clone),
                captured: Arc::clone(&tools_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_thread_store(store);

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    // 迭代上限耗尽 → MaxIterationsExceeded 错误（fork resume 上限 = 200）
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("child_thread_id") && err.contains("execution failed"),
        "错误文本应带 child_thread_id 前缀（可恢复）: {}",
        err
    );
    assert_eq!(
        llm_calls.load(std::sync::atomic::Ordering::SeqCst),
        200,
        "fork resume 迭代上限应为 200（与 execute_fork.rs 一致）"
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

    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "resume-agent", None, Vec::new()).await;

    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools_capture_clone = Arc::clone(&tools_capture);
    struct ResumeFilterLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ResumeFilterLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            Ok(Reasoning::with_answer("", "resume-filter-done"))
        }
    }

    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Agent")];
    let t = SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ResumeFilterLLM {
                captured: Arc::clone(&tools_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_thread_store(store);

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
