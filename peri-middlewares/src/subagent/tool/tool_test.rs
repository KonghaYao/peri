use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        react::{ReactLLM, Reasoning},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    tools::BaseTool,
};
use tempfile::tempdir;

use super::*;
use crate::claude_agent_parser::ToolsValue;

// Mock LLM: returns final answer directly
struct EchoLLM;

#[async_trait::async_trait]
impl ReactLLM for EchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<peri_agent::llm::types::StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

fn make_tool(name: &'static str) -> Arc<dyn BaseTool> {
    struct DummyTool(&'static str);

    #[async_trait::async_trait]
    impl BaseTool for DummyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(format!("{} result", self.0))
        }
    }

    Arc::new(DummyTool(name))
}

fn make_subagent_tool(parent_tools: Vec<Arc<dyn BaseTool>>) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
}

#[test]
fn test_tool_name() {
    let t = make_subagent_tool(vec![]);
    assert_eq!(t.name(), "Agent");
}

#[test]
fn test_agent_parameters_required_is_prompt_only() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let required = params["required"].as_array().unwrap();
    let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"prompt"));
    assert!(!names.contains(&"agent_id"));
    assert!(!names.contains(&"task"));
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
        .invoke(serde_json::json!({
            "subagent_type": "test-agent",
            "cwd": dir.path().to_str().unwrap()
        }))
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
        .invoke(serde_json::json!({
            "prompt": "do something"
        }))
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
        .invoke(serde_json::json!({
            "subagent_type": "fork",
            "prompt": "do something"
        }))
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
        .invoke(serde_json::json!({
            "subagent_type": "nonexistent-agent",
            "prompt": "do something",
            "cwd": "/tmp"
        }))
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("cannot find"),
        "Should return not found error: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_tool_filter_inherit_all() {
    // tools is Empty -> inherit all parent tools, but exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Agent"), // this should be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Write"));
    assert!(!names.contains(&"Agent"), "Agent should not be inherited");
}

#[test]
fn test_tool_filter_allowlist() {
    // tools has value -> only keep specified tools
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["Read".to_string(), "Glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Glob"));
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );
}

#[test]
fn test_tool_filter_disallow() {
    // disallowedTools -> exclude from inherited set
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Edit")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded"
    );
}

#[test]
fn test_tool_filter_wildcard_star() {
    // tools: "*" -> inherit all parent tools (same as Empty), but still exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Bash"),
        make_tool("Agent"), // should still be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Read should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Write"),
        "Write should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Bash"),
        "Bash should be inherited with tools: *"
    );
    assert!(
        !names.contains(&"Agent"),
        "Agent should still be excluded even with tools: *"
    );
}

#[test]
fn test_tool_filter_wildcard_star_with_disallowed() {
    // tools: "*" + disallowedTools -> inherit all except disallowed
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Edit"),
        make_tool("Bash"),
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be inherited");
    assert!(names.contains(&"Bash"), "Bash should be inherited");
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded even with tools: *"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded even with tools: *"
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
        .invoke(serde_json::json!({
            "subagent_type": "test-agent",
            "prompt": "hello",
            "cwd": dir.path().to_str().unwrap()
        }))
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
        .invoke(serde_json::json!({
            "prompt": "hello",
            "subagent_type": "test-agent",
            "description": "test desc",
            "name": "test-alias",
            "isolation": "worktree",
            "run_in_background": true,
            "cwd": dir.path().to_str().unwrap()
        }))
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

/// Recursion prevention: even if agent.md tools field explicitly includes Agent, it must be excluded
#[test]
fn test_agent_excluded_even_when_explicitly_allowed() {
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Agent"), // parent tool set has Agent
    ];
    let t = make_subagent_tool(parent_tools);

    // agent.md has tools: ["Agent", "Read"]
    let allowed = ToolsValue::List(vec!["Agent".to_string(), "Read".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be kept");
    assert!(
        !names.contains(&"Agent"),
        "Agent must be excluded even when explicitly in allowlist (recursion prevention)"
    );
}

/// tools/disallowedTools filtering: case-insensitive (users often write PascalCase)
#[test]
fn test_tool_filter_case_insensitive() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    // User writes different cases in agent.md: tools: READ, glob
    let allowed = ToolsValue::List(vec!["READ".to_string(), "glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Case-insensitive: READ should match Read"
    );
    assert!(
        names.contains(&"Glob"),
        "Case-insensitive: glob should match Glob"
    );
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );

    // disallowedTools case-insensitive
    let allowed2 = ToolsValue::Empty;
    let disallowed2 = ToolsValue::List(vec!["WRITE".to_string()]);
    let filtered2 = t.filter_tools(&allowed2, &disallowed2);
    let names2: Vec<&str> = filtered2.iter().map(|t| t.name()).collect();

    assert!(names2.contains(&"Read"));
    assert!(names2.contains(&"Glob"));
    assert!(
        !names2.contains(&"Write"),
        "WRITE should case-insensitively exclude Write"
    );
}

/// Recursion prevention: Agent in disallowedTools is redundant but should not error
#[test]
fn test_agent_excluded_when_in_disallowed() {
    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Agent".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(!names.contains(&"Agent"), "Agent should not appear");
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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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
        .invoke(serde_json::json!({
            "subagent_type": "tone-test",
            "prompt": "hello",
            "cwd": dir.path().to_str().unwrap()
        }))
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

    // LLM searches all messages for skill content in tool results
    struct SkillPreloadCheckLLM;
    #[async_trait::async_trait]
    impl ReactLLM for SkillPreloadCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let found = messages.iter().any(|m| m.content().contains("Test Skill"));
            Ok(Reasoning::with_answer(
                "",
                if found {
                    "skill_preload_found"
                } else {
                    "skill_preload_not_found"
                },
            ))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| {
            Box::new(SkillPreloadCheckLLM) as Box<dyn ReactLLM + Send + Sync>
        }),
        dir.path().to_str().unwrap().to_string(),
    );

    let result = t
        .invoke(serde_json::json!({
            "subagent_type": "skill-user",
            "prompt": "test task",
            "cwd": dir.path().to_str().unwrap()
        }))
        .await
        .unwrap();

    assert!(
        result.contains("skill_preload_found"),
        "LLM should receive message containing 'preloaded skill file', actual result: {}",
        result
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
    );
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "You are a reviewer.");
    assert_eq!(ov.tone.as_deref().unwrap(), "Be thorough.");
    assert_eq!(ov.proactiveness.as_deref().unwrap(), "Proactively suggest.");
}

#[test]
fn test_overrides_from_agent_def_empty() {
    let ov = SubAgentTool::overrides_from_agent_def("", &None, &None);
    assert!(ov.is_none(), "All-empty fields should return None");
}

#[test]
fn test_overrides_from_agent_def_persona_only() {
    let ov = SubAgentTool::overrides_from_agent_def("I am a helper.", &None, &None);
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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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
        .invoke(serde_json::json!({
            "subagent_type": "forever",
            "prompt": "run",
            "cwd": dir.path().to_str().unwrap()
        }))
        .await
        .unwrap();
    assert!(
        result.contains("interrupted"),
        "Cancellation should cause interrupt message, actual: {}",
        result
    );
}

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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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
        .invoke(serde_json::json!({
            "fork": true,
            "prompt": "do the thing"
        }))
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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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

    t.invoke(serde_json::json!({
        "fork": true,
        "prompt": "check tools"
    }))
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

/// Fork without parent_messages returns error
#[tokio::test]
async fn test_fork_without_parent_messages_returns_error() {
    let t = make_subagent_tool(vec![]);

    let result = t
        .invoke(serde_json::json!({
            "fork": true,
            "prompt": "do something"
        }))
        .await;

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("parent_messages is not set")
            || err_msg.contains("parent message history"),
        "Fork without parent_messages should return error, got: {}",
        err_msg
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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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

    t.invoke(serde_json::json!({
        "fork": true,
        "prompt": "check system"
    }))
    .await
    .unwrap();

    let captured = sys_capture.lock().unwrap();
    assert!(
        captured.contains("FORK-TEST-SYSTEM"),
        "Fork system prompt should contain builder output, got: {}",
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
            _streaming: Option<peri_agent::llm::types::StreamingContext>,
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

    t.invoke(serde_json::json!({
        "fork": true,
        "prompt": "my directive task"
    }))
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

// ─── build_subagent_middlewares 单元测试 ───────────────────────────────────

use super::{build_subagent_middlewares, SubAgentMiddlewareConfig};

#[test]
fn test_build_middleware_fork_config_无_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork("/tmp"));
    assert_eq!(middlewares.len(), 3);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec!["AgentsMdMiddleware", "SkillsMiddleware", "TodoMiddleware"]
    );
}

#[test]
fn test_build_middleware_agent_def_空技能_无_skill_preload() {
    let middlewares =
        build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(vec![], "/tmp"));
    assert_eq!(middlewares.len(), 3);
    assert!(!middlewares
        .iter()
        .any(|m| m.name() == "SkillPreloadMiddleware"));
}

#[test]
fn test_build_middleware_agent_def_有技能_包含_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["test-skill".to_string()],
        "/tmp",
    ));
    assert_eq!(middlewares.len(), 4);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "TodoMiddleware"
        ]
    );
}

#[test]
fn test_build_middleware_顺序固定() {
    // 有 skills 时验证完整顺序
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["a".to_string()],
        "/tmp",
    ));
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "TodoMiddleware"
        ]
    );
}

// ─── frozen 数据传递测试 ──────────────────────────────────────────────────

use peri_agent::agent::state::AgentState;
use peri_agent::agent::State as _;
use tempfile::TempDir;

/// 验证：传入 frozen CLAUDE.md 内容时，SubAgent 中间件链的 AgentsMdMiddleware
/// 应直接 prepend frozen 内容，跳过磁盘读取。
///
/// 这是 SC#2 修复的核心契约：SubAgent 必须复用 main agent 捕获的 frozen 数据，
/// 不能在 spawn 时重新读盘。
#[tokio::test]
async fn test_subagent_中间件链_注入_frozen_claude_md() {
    // Arrange: 空白 tempdir（无 CLAUDE.md），但提供 frozen 内容
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    let frozen_content =
        "# FROZEN TEST CLAUDE.md\nThis content must be injected verbatim.".to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd).with_frozen(
        Some(frozen_content.clone()),
        None,
        None,
    );

    // Act: 构造中间件链，模拟 SubAgent spawn
    let middlewares = build_subagent_middlewares(config);
    let mut state = AgentState::new(&cwd);

    // 找到 AgentsMdMiddleware 并执行 before_agent
    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .expect("AgentsMdMiddleware 必须在链首");
    agents_md.before_agent(&mut state).await.unwrap();

    // Assert: state.messages 头部应有 System(frozen_content)
    let head = state.messages().first().expect("应有 frozen 注入的消息");
    assert!(head.is_system(), "frozen CLAUDE.md 应以 System 消息注入");
    assert!(
        head.content().contains("FROZEN TEST CLAUDE.md"),
        "frozen 内容应原样注入，实际：{}",
        head.content()
    );
}

/// 验证：未提供 frozen 数据时（遗留/测试场景），中间件回退到磁盘读取。
/// 在空白 tempdir 中不注入任何 System 消息。
#[tokio::test]
async fn test_subagent_中间件链_无_frozen_回退磁盘() {
    // Arrange: 空白 tempdir，无 frozen 数据
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd);
    let middlewares = build_subagent_middlewares(config);
    let mut state = AgentState::new(&cwd);

    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .unwrap();
    agents_md.before_agent(&mut state).await.unwrap();

    // Assert: 没有 CLAUDE.md 时，不注入任何 System 消息
    assert!(
        state.messages().is_empty(),
        "无 frozen + 无磁盘文件时不应注入消息"
    );
}
