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
            SubagentLlmSource::prebuilt(Box::new(ForkTestLLM {
                msg_count: Arc::clone(&msg_capture_clone),
            }))
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
            SubagentLlmSource::prebuilt(Box::new(ToolsCheckLLM {
                captured: Arc::clone(&tools_capture_clone),
            }))
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

/// Fork system prompt is consistent with system_builder（H1：经生产装配的
/// bridge base system 捕获最终请求面）
#[tokio::test]
async fn test_fork_system_prompt_consistent() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let model = super::mock_model::RecordingModel::new("sys-check");
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
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

    let captured = model.last_system();
    assert!(
        captured.contains("FORK-TEST-SYSTEM"),
        "Fork system prompt should contain builder output, got: {}",
        captured
    );
}

/// [回归测试] SubAgent fork 的身份来自 `system_builder` 的子能力投影，
/// **不复制父冻结字节**（H1/M3）。
///
/// 历史（审计 prompt-sections-audit.md 条目 7）fork 曾优先复用父冻结
/// system prompt；H2/H3 起父冻结输入只能作为投影输入，逐字继承会把父能力
/// （HITL/子代理声明）带进子请求面。本测试经生产 bridge 捕获最终请求面：
/// 身份恰一次（base system），且不含父字节。
#[tokio::test]
async fn test_fork_identity_is_projected_not_parent_bytes() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let model = super::mock_model::RecordingModel::new("frozen-check");
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_system_builder(Arc::new(|_ov, _cwd| "BUILDER-SYSTEM-PROMPT".to_string()));

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check projected prefix"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let system = model.last_system();
    assert!(
        system.contains("BUILDER-SYSTEM-PROMPT"),
        "fork 身份应来自 system_builder 投影, got: {system}"
    );
    assert!(
        !system.contains("FROZEN-PARENT-SYSTEM-PROMPT"),
        "不能继承父冻结字节, got: {system}"
    );
    // 身份恰一次：对话体不应再出现身份文本（H1 起不写 transcript）。
    let texts = super::mock_model::conversation_texts(&model.last_messages());
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.contains("BUILDER-SYSTEM-PROMPT"))
            .count(),
        0,
        "identity must appear exactly once (base system only): {texts:?}"
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
            SubagentLlmSource::prebuilt(Box::new(DirectiveCheckLLM {
                last: Arc::clone(&last_capture_clone),
            }))
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

// ─── 子链继承策略（单一权威）决策单测：不需要 durable 执行 ───────────────────

/// 夹具工具：可按名与可信 builtin 实例身份声明排除面。
struct CapabilityTool {
    name: &'static str,
    builtin_instance: Option<&'static str>,
}

#[async_trait::async_trait]
impl BaseTool for CapabilityTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "capability fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn builtin_mcp_instance(&self) -> Option<&str> {
        self.builtin_instance
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

fn capability_tools() -> Vec<Arc<dyn BaseTool>> {
    vec![
        Arc::new(CapabilityTool {
            name: "Agent",
            builtin_instance: None,
        }),
        Arc::new(CapabilityTool {
            name: "AskUserQuestion",
            builtin_instance: None,
        }),
        Arc::new(CapabilityTool {
            name: "Workflow",
            builtin_instance: None,
        }),
        Arc::new(CapabilityTool {
            name: "cron_list",
            builtin_instance: Some("cron"),
        }),
        // 用户 MCP 工具恰好叫 Workflow / cron 名字：同样是 fail-closed 排除，
        // 但**不是**按名字归因 builtin——非 builtin 的 cron 同样排除（名字面）。
        Arc::new(CapabilityTool {
            name: "mcp__user__cron",
            builtin_instance: None,
        }),
        Arc::new(CapabilityTool {
            name: "Read",
            builtin_instance: None,
        }),
        Arc::new(CapabilityTool {
            name: "Bash",
            builtin_instance: Some("workspace"),
        }),
    ]
}

/// 继承策略：Agent / AskUser / Workflow / builtin cron 全部闭合；工作区工具保持。
#[test]
fn child_inheritance_filter_closes_extension_faces_and_keeps_workspace() {
    use crate::subagent::fork::child_inheritance_filter;
    let filter = child_inheritance_filter();
    let tools = capability_tools();
    let allowed: Vec<&str> = tools
        .iter()
        .filter(|tool| filter(tool.as_ref()))
        .map(|tool| tool.name())
        .collect();
    assert_eq!(
        allowed,
        vec!["mcp__user__cron", "Read", "Bash"],
        "只有非扩展面的工具有效"
    );
}

/// 定义型与 fork 共用同一策略：`canonical_tool_filter`（含 `*` 继承）与
/// `filter_tools` 结果一致闭合。
#[test]
fn definition_and_fork_share_one_inheritance_policy() {
    use crate::subagent::fork::{canonical_tool_filter, filter_tools};
    use peri_mcp_core::agent_definition::ToolsValue;
    let tools = capability_tools();
    let star = ToolsValue::List(vec!["*".to_string()]);
    let definition_filter = canonical_tool_filter(&star, &ToolsValue::Empty);
    let names: Vec<&str> = tools
        .iter()
        .filter(|tool| definition_filter(tool.as_ref()))
        .map(|tool| tool.name())
        .collect();
    assert_eq!(names, vec!["mcp__user__cron", "Read", "Bash"]);

    let filtered = filter_tools(&tools, &star, &ToolsValue::Empty);
    let filtered_names: Vec<&str> = filtered.iter().map(|tool| tool.name()).collect();
    assert_eq!(
        filtered_names,
        vec!["mcp__user__cron", "Read", "Bash"],
        "filter_tools 与 canonical_tool_filter 同一决策"
    );
}

/// fork 生成的 spawn config 使用同一策略：前台/后台路径传入的过滤在组合后
/// 仍然闭合扩展面（直接检查 `SubagentSpawnConfig.tool_filter` 决策）。
#[tokio::test]
async fn fork_spawn_config_filter_closes_extension_tools() {
    use peri_agent::session::subagent::{ForkDirectiveKind, SubagentCancelPolicy, SubagentRunMode};
    let model = super::mock_model::RecordingModel::new("done");
    let parent_tools = Arc::new(capability_tools());
    let tool = SubAgentTool::new(
        Arc::clone(&parent_tools),
        None,
        Arc::new({
            let model = Arc::clone(&model);
            move |_: Option<&str>| {
                SubagentLlmSource::model(model.clone() as Arc<dyn peri_model::Model>, "mock-model")
            }
        }),
        "/tmp".to_string(),
    );
    let config = tool.spawn_config_base(
        "fork".to_string(),
        "do it".to_string(),
        Vec::new(),
        SubagentCancelPolicy::Cascade,
        200,
        Some(ForkDirectiveKind::Fork),
        SubagentRunMode::Sync,
        SubagentLlmSource::model(
            Arc::clone(&model) as Arc<dyn peri_model::Model>,
            "mock-model",
        ),
        parent_tools.iter().cloned().collect(),
        crate::subagent::fork::child_inheritance_filter(),
        Some("FORK_IDENTITY".to_string()),
        Vec::new(),
        "/tmp".to_string(),
        None,
    );
    for tool in parent_tools.iter() {
        let kept = (config.tool_filter)(tool.as_ref());
        match tool.name() {
            "Agent" | "AskUserQuestion" | "Workflow" | "cron_list" => {
                assert!(!kept, "{} 不得进入 fork 工具面", tool.name())
            }
            _ => assert!(kept, "{} 应保持可继承", tool.name()),
        }
    }
}

/// deferred 面同样闭合：会话工具目录（ToolSearch 索引来源）用同一
/// `tool_filter` 构建 published 视图，被策略拒绝的扩展面不会经 deferred
/// 搜索/执行重新可见。
#[test]
fn child_inheritance_filter_closes_deferred_catalog_view() {
    let tools = capability_tools();
    let mut base = std::collections::BTreeMap::new();
    for tool in tools {
        base.insert(tool.name().to_string(), tool);
    }
    let catalog = peri_agent::session::tool_catalog::SessionToolCatalog::with_filter(
        base,
        None,
        crate::subagent::fork::child_inheritance_filter(),
    );
    let names: Vec<String> = catalog.snapshot().tools.keys().cloned().collect();
    for excluded in ["Agent", "AskUserQuestion", "Workflow", "cron_list"] {
        assert!(
            !names.iter().any(|name| name == excluded),
            "{excluded} 不得进入 deferred 目录视图: {names:?}"
        );
    }
    assert!(names.iter().any(|name| name == "Read"), "{names:?}");
}
