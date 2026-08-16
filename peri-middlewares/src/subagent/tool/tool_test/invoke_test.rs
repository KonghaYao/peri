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

    let t = make_subagent_tool(vec![]);
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
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages);

    // subagent_type: "fork" should trigger fork mode, NOT try to load an agent named "fork"
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "fork",
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    let t = make_subagent_tool(vec![]);
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
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = make_subagent_tool(vec![]);
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
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Reserved fields don't affect execution, should still return normal result
    assert!(
        result.contains("echo"),
        "Should execute normally: {}",
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
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("tone-test.md"),
        "---\nname: tone-test\ndescription: Test tone injection\n---\n\nYou are a tone tester.\n",
    )
    .unwrap();

    // LLM echoes system message content
    struct SystemEchoLLM;
    #[async_trait::async_trait]
    impl ReactLLM for SystemEchoLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            // Find system message and return its content
            let system_content = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_else(|| "no-system".to_string());
            Ok(Reasoning::with_answer(
                "",
                format!("system={system_content}"),
            ))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| Box::new(SystemEchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_system_builder(Arc::new(|_overrides, _cwd| "tone: be concise".to_string()));

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "tone-test",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("tone: be concise"),
        "System prompt should be injected: {}",
        result
    );
}

/// Verify SkillPreloadMiddleware is correctly registered when agent.md contains skills field
/// LLM received messages should contain "(system: preloaded skill file)"
#[tokio::test]
async fn test_skill_preload_registered() {
    let dir = tempdir().unwrap();
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

    // SKILL.md content
    std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
        )
        .unwrap();

    // LLM 验证 prompt 已由 Receive 写入、并精确统计显式 skill 的 fake ToolResult。
    let preload_count: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let preload_count_clone = Arc::clone(&preload_count);
    struct SkillPreloadCheckLLM {
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for SkillPreloadCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
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
            Ok(Reasoning::with_answer("", "skill_preload_found"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(SkillPreloadCheckLLM {
                preload_count: Arc::clone(&preload_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        dir.path().to_str().unwrap().to_string(),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "prompt": "test task",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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

/// Verify overrides_from_agent_def correctly extracts AgentOverrides from parsed data
#[test]
fn test_overrides_from_agent_def_with_all_fields() {
    let ov = SubAgentTool::overrides_from_agent_def(
        "You are a reviewer.",
        &Some("Be thorough.".to_string()),
        &Some("Proactively suggest.".to_string()),
        &None,
    );
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "You are a reviewer.");
    assert_eq!(ov.tone.as_deref().unwrap(), "Be thorough.");
    assert_eq!(ov.proactiveness.as_deref().unwrap(), "Proactively suggest.");
}

#[test]
fn test_overrides_from_agent_def_empty() {
    let ov = SubAgentTool::overrides_from_agent_def("", &None, &None, &None);
    assert!(ov.is_none(), "All-empty fields should return None");
}

#[test]
fn test_overrides_from_agent_def_persona_only() {
    let ov = SubAgentTool::overrides_from_agent_def("I am a helper.", &None, &None, &None);
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "I am a helper.");
    assert!(ov.tone.is_none());
    assert!(ov.proactiveness.is_none());
}

/// Verify cancellation token can interrupt sub-agent execution
#[tokio::test]
async fn test_cancel_token_interrupts_subagent() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("forever.md"),
        "---\nname: forever\ndescription: Runs forever\n---\n\nYou run forever.\n",
    )
    .unwrap();

    // LLM always calls a never-registered tool, causing ToolNotFound but no infinite loop
    struct ToolNotFoundLLM;
    #[async_trait::async_trait]
    impl ReactLLM for ToolNotFoundLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            if messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }))
            {
                Ok(Reasoning::with_answer("", "done"))
            } else {
                Ok(Reasoning::with_tools(
                    "call missing",
                    vec![peri_agent::agent::react::ToolCall::new(
                        "id1",
                        "nonexistent",
                        serde_json::json!({}),
                    )],
                ))
            }
        }
    }

    let cancel = AgentCancellationToken::new();
    // Trigger cancellation before sub-agent execution
    cancel.cancel();

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| Box::new(ToolNotFoundLLM) as Box<dyn ReactLLM + Send + Sync>),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_cancel(cancel);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "forever",
                "prompt": "run",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("interrupted"),
        "Cancellation should cause interrupt message, actual: {}",
        result
    );
}
