use super::*;

#[test]
fn test_tool_name() {
    let t = make_subagent_tool(vec![]);
    assert_eq!(t.name(), "Agent");
}

#[test]
fn test_agent_parameters_required_is_empty_for_resume() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    // resume_thread_id 存在时 prompt 可缺省（隐式 continue），required 恒空；
    // 非 resume 路径缺 prompt 由 invoke 运行时校验兜底（test_agent_prompt_missing_returns_error）
    let required = params["required"].as_array().unwrap();
    assert!(
        required.is_empty(),
        "required 应为空数组（resume 时 prompt 可缺省），实际: {:?}",
        required
    );
    // resume_thread_id 参数已声明（string 类型）
    assert!(
        params["properties"]["resume_thread_id"]["type"] == "string",
        "resume_thread_id 应为 string 类型参数"
    );
}

#[test]
fn test_agent_fork_description_declares_exclusivity_with_subagent_type() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let fork_desc = params["properties"]["fork"]["description"]
        .as_str()
        .unwrap();
    assert!(
        fork_desc.contains("Mutually exclusive with subagent_type"),
        "fork 描述应声明与 subagent_type 互斥，实际: {fork_desc}"
    );
}

/// Verify error returned when prompt parameter is missing
#[tokio::test]
async fn test_agent_prompt_missing_returns_error() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = with_agent_face(make_subagent_tool(vec![]), dir.path()).await;
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("prompt"),
        "Should return missing prompt error: {}",
        err_msg
    );
}

/// Verify error returned when subagent_type parameter is missing and fork is not set
#[tokio::test]
async fn test_agent_subagent_type_missing_returns_error() {
    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("subagent_type") || err_msg.contains("fork"),
        "Should return missing subagent_type error with fork hint: {}",
        err_msg
    );
}

/// Verify subagent_type="fork" is treated as fork:true (common LLM mistake)
#[tokio::test]
async fn test_subagent_type_fork_treated_as_fork_mode() {
    let host = HostFixture::open("fixture-invoke-fork").await;
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));

    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(|_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(EchoLLM),
                    "fixture-scripted",
                )
            }),
            host.cwd.clone(),
        )
        .with_parent_messages(parent_messages),
    );

    // subagent_type: "fork" should trigger fork mode, NOT try to load an agent named "fork"
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "fork",
                "prompt": "do something"
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    assert!(
        result.contains("echo") || result.contains("Fork") || result.contains("fork-done"),
        "subagent_type='fork' should trigger fork mode: {}",
        result
    );
}

#[tokio::test]
async fn test_tool_agent_not_found() {
    // W5：未命中判定来自资源面（空 agent 根 ⇒ 无候选），不依赖磁盘兜底。
    let dir = tempdir().unwrap();
    let t = with_agent_face(make_subagent_tool(vec![]), dir.path()).await;
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "nonexistent-agent",
                "prompt": "do something",
                "cwd": "/tmp"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("cannot find"),
        "Should return not found error: {}",
        err_msg
    );
}
#[tokio::test]
async fn test_tool_executes_with_valid_agent_file() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-valid").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = host.bind(with_agent_face(make_subagent_tool(vec![]), dir.path()).await);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    // EchoLLM returns echo: hello
    assert!(
        result.contains("echo"),
        "Should receive sub-agent output: {}",
        result
    );
}

/// Verify Agent reserved fields (isolation/run_in_background/description/name) don't affect execution
#[tokio::test]
async fn test_agent_reserved_fields_parsed() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-reserved").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = host.bind(with_agent_face(make_subagent_tool(vec![]), dir.path()).await);
    let result = t
        .invoke(
            serde_json::json!({
                "prompt": "hello",
                "subagent_type": "test-agent",
                "description": "test desc",
                "name": "test-alias",
                "isolation": "worktree",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    // 保留字段不影响执行：durable 后台路径返回可恢复的启动回执
    // （child_thread_id 可继续，任务立即注册）。
    assert!(
        result.contains("Background task bg-") && result.contains("thread:"),
        "Should start normally: {}",
        result
    );
}

#[tokio::test]
async fn test_agent_tool_in_list() {
    // Verify SubAgentTool's tool name is correct, can join tool list
    let t = make_subagent_tool(vec![]);
    assert_eq!(t.name(), "Agent");
    let def = t.definition();
    assert_eq!(def.name, "Agent");
}

/// Verify with_system_builder correctly injects system prompt
#[tokio::test]
async fn test_system_builder_injects_system_message() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-system").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("tone-test.md"),
        "---\nname: tone-test\ndescription: Test tone injection\n---\n\nYou are a tone tester.\n",
    )
    .unwrap();

    // H1：经生产 bridge 捕获最终请求 system（身份投影）。
    let model = super::mock_model::RecordingModel::new("system-check");
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_system_builder(Arc::new(|_overrides, _cwd| "tone: be concise".to_string()));
    let t = host.bind(with_agent_face(t, dir.path()).await);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "tone-test",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    let system = model.last_system();
    assert!(
        system.contains("tone: be concise"),
        "System prompt should be injected via bridge base system: {system}"
    );
    assert!(
        result.contains("system-check"),
        "subagent should complete: {result}"
    );
}

/// Verify SkillPreloadMiddleware is correctly registered when agent.md contains skills field
/// LLM received messages should contain "(system: preloaded skill file)"
#[tokio::test]
async fn test_skill_preload_registered() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-skill").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();

    // agent.md with skills field
    std::fs::write(
            agents_dir.join("skill-user.md"),
            "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
        )
        .unwrap();

    // 磁盘上的 SKILL.md 是**反例**：W4b 后子链预载只查 MCP registry，
    // 宿主不再读这个文件（内容差异即证明未回落磁盘）。
    std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nDISK CONTENT MUST NOT BE READ.\n",
        )
        .unwrap();

    // LLM 验证 prompt 已由 Receive 写入、并精确统计显式 skill 的 fake ToolResult。
    let preload_count: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let preload_count_clone = Arc::clone(&preload_count);
    #[derive(Clone)]
    struct SkillPreloadCheckLLM {
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    impl SkillPreloadCheckLLM {
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

            assert!(
                messages
                    .iter()
                    .any(|message| message.content().contains("test task")),
                "before_agent must run after Receive has appended the prompt"
            );
            *self.preload_count.lock().unwrap() = messages
                .iter()
                .filter(|message| {
                    message
                        .content()
                        .contains("This is the test skill content.")
                })
                .count();
            text_events("skill_preload_found")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(SkillPreloadCheckLLM);

    let t = super::with_skill_registry(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(SkillPreloadCheckLLM {
                        preload_count: Arc::clone(&preload_count_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            dir.path().to_str().unwrap().to_string(),
        ),
        "workspace",
        &[("test-skill", "This is the test skill content.\n")],
    );
    let t = host.bind(with_agent_face(t, dir.path()).await);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "prompt": "test task",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();

    assert!(
        result.contains("skill_preload_found"),
        "LLM should receive message containing 'preloaded skill file', actual result: {}",
        result
    );
    assert_eq!(
        *preload_count.lock().unwrap(),
        1,
        "the explicit skill must inject exactly one ToolResult sequence"
    );
}

#[test]
fn test_agent_description_extended() {
    let t = make_subagent_tool(vec![]);
    let desc = t.description();
    assert!(
        desc.contains("Usage:"),
        "description should contain Usage section"
    );
    assert!(
        desc.contains("sub-agent") || desc.contains("sub agent"),
        "description should mention sub-agent"
    );
    assert!(
        desc.contains("isolated") || desc.contains("isolation"),
        "description should mention context isolation"
    );
    assert!(
        desc.contains("Fork mode"),
        "description should mention Fork mode"
    );
    assert!(
        desc.len() > 300,
        "description should be extended multi-paragraph text"
    );
}

/// Verify cancellation token can interrupt sub-agent execution
#[tokio::test]
async fn test_cancel_token_interrupts_subagent() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-cancel").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("forever.md"),
        "---\nname: forever\ndescription: Runs forever\n---\n\nYou run forever.\n",
    )
    .unwrap();

    // LLM always calls a never-registered tool, causing ToolNotFound but no infinite loop
    #[derive(Clone)]
    struct ToolNotFoundLLM;
    impl ToolNotFoundLLM {
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

            if messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }))
            {
                text_events("done")
            } else {
                tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
                    "id1",
                    "nonexistent",
                    serde_json::json!({}),
                )])
            }
        }
    }
    crate::subagent::test_support::fixture_model_impl!(ToolNotFoundLLM);

    let cancel = AgentCancellationToken::new();
    // Trigger cancellation before sub-agent execution（Cascade 语义下子链取消
    // 来自父会话 token）
    host.cancel_parent();
    cancel.cancel();

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(ToolNotFoundLLM),
                "fixture-scripted",
            )
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_cancel(cancel);
    let t = host.bind(with_agent_face(t, dir.path()).await);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "forever",
                "prompt": "run",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    assert!(
        result.contains("interrupted"),
        "Cancellation should cause interrupt message, actual: {}",
        result
    );
}

/// MCP 同步限制早于后台缺 owner 的降级和 fork 分支；同步 fork 仍忽略 type。
#[tokio::test]
async fn test_agent_invoke_mcp_background_rejection_precedes_fork_fallback() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-invoke-mcp-bg").await;
    let tool = host.bind(make_subagent_tool(vec![]));
    let messages = vec![BaseMessage::human("parent context")];
    let mut input = serde_json::json!({
        "subagent_type": "mcp__missing__agent",
        "fork": true,
        "run_in_background": true,
        "prompt": "fork task",
        "cwd": dir.path().to_str().unwrap()
    });
    let error = tool
        .invoke(input.clone(), host.context(&messages))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: MCP Agents currently support synchronous activation only"
    );
    input["run_in_background"] = serde_json::json!(false);
    let result = tool.invoke(input, host.context(&messages)).await.unwrap();
    assert!(
        result.contains("fork task"),
        "同步 fork 不应尝试远端 definition 激活：{result}"
    );
}

/// 父 host 整体覆盖 builder 回退：空 task_manager 也不能从旧 host 拼回后台能力。
#[tokio::test]
async fn test_agent_invoke_parent_host_masks_fallback_runtime_and_store() {
    let dir = tempdir().unwrap();
    let _host = HostFixture::open_in(dir.path(), "fixture-invoke-parent-host").await;
    let fallback_dir = tempdir().unwrap();
    let store = SessionFixture::open_in(dir.path()).await;
    let fallback_store = SessionFixture::open_in(fallback_dir.path()).await;
    // 父 host 必须带完整父事实：门面 + root owner + 父 thread id（child 保存的前置条件）。
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    let parent = peri_agent::session::Session::new(
        Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    parent.set_subagent_host(peri_agent::session::subagent::SubagentHost {
        session_resources: Some(store.facade()),
        parent_thread_id: Some(parent_id.clone()),
        ..Default::default()
    });
    // 本次委派的 tool-call 身份（父侧 provenance 来源）。
    let invocation_id = "fixture-mask-invocation";
    let fallback_manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let tool = make_subagent_tool(vec![])
        .with_session_resources(fallback_store.facade())
        .with_task_manager(fallback_manager.clone())
        .with_parent_thread_id(parent_id.clone())
        .with_parent_session(parent);
    let mut ctx = peri_agent::tools::ToolContext::new(&[], &cwd);
    ctx.invocation_id = Some(invocation_id.to_string());
    ctx.tool_call_id = Some(invocation_id.to_string());
    let result = tool.invoke(
        serde_json::json!({"fork": true, "run_in_background": true, "prompt": "sync fallback", "cwd": cwd.clone()}),
        ctx,
    ).await.unwrap();
    let thread_id = result
        .lines()
        .next()
        .unwrap()
        .strip_prefix("child_thread_id: ")
        .unwrap()
        .to_string();
    // 子代理为 hidden，用户可见的 list_threads 会过滤它；直接核对真实持久化记录。
    let meta = store.load_meta(&thread_id).await.unwrap();
    assert!(meta.hidden);
    assert_eq!(meta.agent_status, peri_agent::thread::AgentStatus::Done);
    assert!(result.contains("sync fallback") && !result.contains("Background task"));
    assert!(fallback_store.load_meta(&thread_id).await.is_err());
    assert_eq!(fallback_manager.active_count(), 0);
}
