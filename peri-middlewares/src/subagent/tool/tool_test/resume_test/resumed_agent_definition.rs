/// agent-def resume：title == agent_id → load_agent_def 重新应用过滤
/// （tools 白名单 + Agent 恒排除；与 fork resume 的"父工具集无过滤"区分；
/// build_result 的 skill_names / system_prompt 被 resume_config_base 丢弃——
/// R-H1 / F4，不重复注入）
#[tokio::test]
async fn test_resume_thread_id_agent_def_refilters_tools() {
    let dir = tempfile::tempdir().unwrap();
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
    let capture = super::install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    super::preset_resumable_thread(
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
            use crate::subagent::test_support;
            let _ = &cancellation;
            let _messages = test_support::base_messages(&request);
            let defined = test_support::defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            test_support::text_events("resume-filter-done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(ResumeFilterLLM);

    let parent_tools = vec![
        super::make_tool("Read"),
        super::make_tool("Write"),
        super::make_tool("Agent"),
    ];
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
    let t = super::with_agent_face(t, dir.path()).await;

    let result = capture
        .invoke_and_wait(
            &t,
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd.clone(),
            }),
            super::preset_child_ctx(&id, "."),
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
    let dir = tempfile::tempdir().unwrap();
    super::write_test_agent(&dir);
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
    let capture = super::install_parent_host(&store, &parent);
    let id = uuid::Uuid::now_v7().to_string();
    super::preset_resumable_thread(&store, &id, "test-agent", Some(parent_id.as_str()), vec![])
        .await;
    let tool = super::with_agent_face(super::make_subagent_tool(vec![]), dir.path())
        .await
        .with_session_resources(store.facade())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent.clone());
    let result = capture
        .invoke_and_wait(
            &tool,
            serde_json::json!({
                "resume_thread_id": format!("  {id}\n"),
                "subagent_type": "mcp__missing__agent",
                "fork": true,
                "model": "invalid-model",
                "prompt": null,
                "cwd": cwd.clone()
            }),
            super::preset_child_ctx(&id, "."),
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
