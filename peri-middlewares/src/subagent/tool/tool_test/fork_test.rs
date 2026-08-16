use super::*;

// ─── Fork path tests ────────────────────────────────────────────────────

/// Fork inherits parent messages
#[tokio::test]
async fn test_fork_inherits_parent_messages() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    parent_messages.write().push(BaseMessage::ai("Hi there"));

    let msg_capture: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let msg_capture_clone = Arc::clone(&msg_capture);

    struct ForkTestLLM {
        msg_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ForkTestLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.msg_count.lock().unwrap() = messages.len();
            Ok(Reasoning::with_answer("", "fork-done"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ForkTestLLM {
                msg_count: Arc::clone(&msg_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(Arc::clone(&parent_messages));

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("fork-done"),
        "Fork should execute: {}",
        result
    );
    // Messages should include: 2 parent history + 1 system + 1 fork directive (human) = 4+
    let count = *msg_capture.lock().unwrap();
    assert!(
        count >= 3,
        "Fork should receive parent messages (got {})",
        count
    );
}

/// Fork registers all tools including Agent (no hard-coded exclusion)
#[tokio::test]
async fn test_fork_registers_all_tools_including_agent() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools_capture_clone = Arc::clone(&tools_capture);

    struct ToolsCheckLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ToolsCheckLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            Ok(Reasoning::with_answer("", "tools-check"))
        }
    }

    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];

    let t = SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ToolsCheckLLM {
                captured: Arc::clone(&tools_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages);

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check tools"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let captured = tools_capture.lock().unwrap();
    assert!(
        captured.contains(&"Agent".to_string()),
        "Fork should register Agent tool (no exclusion), got: {:?}",
        *captured
    );
    assert!(
        captured.contains(&"Read".to_string()),
        "Fork should register Read tool, got: {:?}",
        *captured
    );
}

/// Fork without parent_messages succeeds with empty ToolContext messages
#[tokio::test]
async fn test_fork_without_parent_messages_returns_error() {
    let t = make_subagent_tool(vec![]);

    // Fork 现在从 ToolContext 获取消息（而非 self.parent_messages），
    // 空消息也是合法输入。
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        result.is_ok(),
        "Fork with empty ToolContext messages should succeed, got: {:?}",
        result.err()
    );
}

/// Fork system prompt is consistent with system_builder
#[tokio::test]
async fn test_fork_system_prompt_consistent() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let sys_capture: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let sys_capture_clone = Arc::clone(&sys_capture);

    struct SystemCheckLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for SystemCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let sys = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_default();
            *self.captured.lock().unwrap() = sys;
            Ok(Reasoning::with_answer("", "sys-check"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(SystemCheckLLM {
                captured: Arc::clone(&sys_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_system_builder(Arc::new(|_ov, _cwd| "FORK-TEST-SYSTEM".to_string()));

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check system"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let captured = sys_capture.lock().unwrap();
    assert!(
        captured.contains("FORK-TEST-SYSTEM"),
        "Fork system prompt should contain builder output, got: {}",
        *captured
    );
}

/// [回归测试] SubAgent fork 复用父冻结 system prompt，不回退 system_builder。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md 条目 7）：fork
/// 生产路径继承父**冻结** system prompt（execute_fork.rs frozen_system_prompt
/// 优先），与主 agent 前缀保持一致；若改为无条件走 system_builder 或每轮
/// 重渲染，会破坏会话内前缀一致性。本测试固定两个输入（frozen 与 builder），
/// 断言 frozen 优先——即"同一 FrozenSessionData 输入下主 agent 与 subagent
/// 复用稳定 prompt"的外部结果。
#[tokio::test]
async fn test_fork_prefers_frozen_system_prompt_over_builder() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let sys_capture: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let sys_capture_clone = Arc::clone(&sys_capture);

    struct FrozenCheckLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for FrozenCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let sys = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_default();
            *self.captured.lock().unwrap() = sys;
            Ok(Reasoning::with_answer("", "frozen-check"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(FrozenCheckLLM {
                captured: Arc::clone(&sys_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_frozen_system_prompt(Arc::new("FROZEN-PARENT-SYSTEM-PROMPT".to_string()))
    .with_system_builder(Arc::new(|_ov, _cwd| "BUILDER-SYSTEM-PROMPT".to_string()));

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check frozen prefix"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let captured = sys_capture.lock().unwrap();
    assert!(
        captured.contains("FROZEN-PARENT-SYSTEM-PROMPT"),
        "Fork 应复用父冻结 system prompt, got: {}",
        *captured
    );
    assert!(
        !captured.contains("BUILDER-SYSTEM-PROMPT"),
        "frozen 存在时不应回退 system_builder, got: {}",
        *captured
    );
}

/// Fork directive includes RULES
#[tokio::test]
async fn test_fork_directive_includes_rules() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let last_capture: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let last_capture_clone = Arc::clone(&last_capture);

    struct DirectiveCheckLLM {
        last: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for DirectiveCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let last = messages.last().map(|m| m.content()).unwrap_or_default();
            *self.last.lock().unwrap() = last;
            Ok(Reasoning::with_answer("", "directive-check"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(DirectiveCheckLLM {
                last: Arc::clone(&last_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages);

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "my directive task"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let last = last_capture.lock().unwrap();
    assert!(
        last.contains("<fork_directive>"),
        "Fork directive should contain <fork_directive>, got: {}",
        *last
    );
    assert!(
        last.contains("RULES"),
        "Fork directive should contain RULES, got: {}",
        *last
    );
    assert!(
        last.contains("my directive task"),
        "Fork directive should contain the prompt, got: {}",
        *last
    );
}
