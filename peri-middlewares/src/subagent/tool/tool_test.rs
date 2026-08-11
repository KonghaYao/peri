use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::{
    agent::{
        events::ExecutorEvent,
        events_v2::ObserveEvent,
        react::{ReactLLM, Reasoning, StreamingContext},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    thread::ThreadStore,
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
        _streaming: Option<StreamingContext>,
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
        fn is_direct(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
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

/// Agent 工具 schema 应声明 `model` 参数（string，可选），并列出全部可用档位
#[test]
fn test_agent_parameters_declares_model_tier() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    assert!(
        params["properties"]["model"]["type"] == "string",
        "model 应为 string 类型参数"
    );
    let desc = params["properties"]["model"]["description"]
        .as_str()
        .unwrap();
    for tier in ["inherit", "haiku", "sonnet", "opus", "fable"] {
        assert!(
            desc.contains(tier),
            "model 描述应列出档位 {}: {}",
            tier,
            desc
        );
    }
}

/// 记录 llm_factory 收到的 model alias 的工具构造（每次 subagent 装配调用一次）
fn make_recording_subagent_tool(
    parent_tools: Vec<Arc<dyn BaseTool>>,
    aliases: Arc<std::sync::Mutex<Vec<Option<String>>>>,
) -> SubAgentTool {
    let aliases_clone = Arc::clone(&aliases);
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |alias: Option<&str>| {
            aliases_clone
                .lock()
                .unwrap()
                .push(alias.map(|s| s.to_string()));
            Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
}

fn write_test_agent_with_model(dir: &tempfile::TempDir, model: &str) {
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        format!(
            "---\nname: test-agent\ndescription: A test agent\nmodel: {}\n---\n\nYou are a test agent.\n",
            model
        ),
    )
    .unwrap();
}

/// model 参数覆盖 frontmatter：定义声明 sonnet，调用传 haiku → llm_factory 收到 "haiku"
#[tokio::test]
async fn test_agent_model_override_replaces_frontmatter() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "haiku",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo"), "应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("haiku".to_string())],
        "调用参数 model 应覆盖 frontmatter"
    );
}

/// model: "inherit" → 继承父模型（llm_factory 收到 None），覆盖 frontmatter
#[tokio::test]
async fn test_agent_model_inherit_uses_parent_model() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "inherit",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo"), "应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "inherit 应继承父模型");
}

/// 省略 model（或传空串 / 纯空白占位符）→ 保持 agent 定义 frontmatter model
#[tokio::test]
async fn test_agent_model_omitted_keeps_frontmatter() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    for model in [None, Some(""), Some("   ")] {
        let mut input = serde_json::json!({
            "subagent_type": "test-agent",
            "prompt": "hello",
            "cwd": dir.path().to_str().unwrap()
        });
        if let Some(m) = model {
            input["model"] = serde_json::Value::String(m.to_string());
        }
        let result = t
            .invoke(input, peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[
            Some("sonnet".to_string()),
            Some("sonnet".to_string()),
            Some("sonnet".to_string())
        ],
        "省略/空/空白 model 应保持 frontmatter 定义"
    );
}

/// 省略 model + frontmatter "inherit"（或空串）→ llm_factory 收到 None（父模型）
#[tokio::test]
async fn test_agent_model_omitted_inherit_or_empty_frontmatter() {
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    for fm in ["inherit", ""] {
        let dir = tempdir().unwrap();
        write_test_agent_with_model(&dir, fm);
        let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
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
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[None, None],
        "frontmatter inherit/空 应继承父模型"
    );
}

/// 档位大小写不敏感："HAIKU" → "haiku"；"InHerit" → 父模型（None）
#[tokio::test]
async fn test_agent_model_case_insensitive() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    for model in ["HAIKU", "InHerit"] {
        let result = t
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "prompt": "hello",
                    "model": model,
                    "cwd": dir.path().to_str().unwrap()
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("haiku".to_string()), None],
        "档位应大小写不敏感归一"
    );
}

/// fork + 未知档位：model 被宽容忽略且不校验（与 resume 忽略语义一致）
#[tokio::test]
async fn test_agent_model_unknown_ignored_on_fork() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_parent_messages(parent_messages);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "turbo"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("fork + 未知档位应成功（model 被宽容忽略）");
    assert!(result.contains("echo"), "fork 应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "fork 恒继承父模型");
}

/// 未知档位拒绝（不静默回退父模型），且不调用 llm_factory
#[tokio::test]
async fn test_agent_model_unknown_rejected() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "turbo",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("invalid model tier"),
        "未知档位应报错而非静默回退: {}",
        err_msg
    );
    assert!(
        err_msg.contains("inherit, haiku, sonnet, opus, fable"),
        "错误信息应列出可用档位: {}",
        err_msg
    );
    assert!(
        aliases.lock().unwrap().is_empty(),
        "未知档位不应调用 llm_factory"
    );
}

/// fork 忽略 model：fork 调用携带 model 仍成功，llm_factory 收到 None（父模型）
#[tokio::test]
async fn test_agent_model_ignored_on_fork() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_parent_messages(parent_messages);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "haiku"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("fork + model 应成功（model 被忽略）");
    assert!(result.contains("echo"), "fork 应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "fork 恒继承父模型");
}

/// resume 不允许 model 覆盖：恢复按 thread title 重建（frontmatter sonnet），
/// 调用传 model 被忽略且不报错（与 subagent_type/fork 同款宽容语义）；
/// 未知档位 "turbo" 同样被宽容忽略，不做校验
#[tokio::test]
async fn test_resume_thread_id_ignores_model_field() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
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

    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases)).with_thread_store(store);
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "model": "turbo",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("resume + model 应容错恢复而非报错");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("sonnet".to_string())],
        "resume 应保持原定义模型，不被调用参数覆盖"
    );
}

/// 后台定义型路径应用 model 覆盖（execute_bg.rs 透传）
#[tokio::test]
async fn test_agent_model_override_applies_to_background() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);

    let invoke_msg = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "bg task",
                "model": "haiku",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("bg 应启动");
    assert!(
        invoke_msg.contains("Background task"),
        "应返回后台任务启动消息: {}",
        invoke_msg
    );
    // llm_factory 在 invoke_background 装配阶段同步调用（spawn 之前）
    {
        let recorded = aliases.lock().unwrap();
        assert_eq!(
            recorded.as_slice(),
            &[Some("haiku".to_string())],
            "bg 定义型路径应应用 model 覆盖"
        );
    }

    // 等待 BackgroundTaskCompleted，避免任务悬挂
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(_))) => break,
            Ok(_) => {}
            _ => break,
        }
    }
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
    let _state = AgentState::new(&cwd);

    // 找到 AgentsMdMiddleware（v2 通过 prompt_contribution 声明贡献，不再 prepend_message）
    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .expect("AgentsMdMiddleware 必须在链首");

    // Assert: prompt_contribution 应返回 frozen 内容
    let contribution = agents_md
        .prompt_contribution()
        .expect("v2 prompt_contribution 应返回 frozen CLAUDE.md 内容");
    assert!(
        contribution.contains("FROZEN TEST CLAUDE.md"),
        "frozen 内容应通过 prompt_contribution 暴露，实际：{}",
        contribution
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

// ─── SubAgent v2 集成测试（4 场景）──────────────────────────────────────
//
// 对应 docs/refactor/pending-fix-plan-2026-07-06.md [INTEGRATION-TESTS]。
// 依赖 BUG-A / BUG-B / BUG-C 修复（已完成）。
// 场景 3（Sync Cascade cancel）已由 `test_cancel_token_interrupts_subagent`（:655）覆盖，
// 这里在文末的 markdown 报告中确认覆盖范围。
//
// 所有测试基于 v2 路径（`build_v2_subagent_context` / `SubAgentTool::invoke`），
// 不依赖 v1 `ReActAgent`。

/// 场景 1（Fork 父消息透传）：端到端验证 fork 模式下子 agent 收到完整父对话历史。
///
/// 断言（对应 plan.md §场景1 acceptance）：
/// - mock LLM 收到的 messages.len() >= 4（3 父 + 1 fork_directive prompt）
///   实际还会包含 system_builder 注入的 1 条 System 消息
/// - mock LLM 收到的最后一条消息是 `BaseMessage::Human` 且 content 包含 `<fork_directive>`（验证 BUG-A）
/// - mock LLM 收到的 messages 包含 `BaseMessage::System` 内容含 "FORK-CONTEXT-SP"（验证 BUG-B fork 路径）
/// - mock LLM 收到的 messages 包含全部 3 条父消息（按顺序透传，验证 BUG-C）
#[tokio::test]
async fn test_integration_fork_parent_messages_passthrough() {
    // Arrange: 3 条父消息（Human/AI 交替 + 1 条 system context）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q1"));
    parent_messages
        .write()
        .push(BaseMessage::ai("parent A1 with details"));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q2 followup"));

    // 捕获 mock LLM 收到的完整消息列表
    let captured: Arc<std::sync::Mutex<Vec<BaseMessage>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct CaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<BaseMessage>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.to_vec();
            Ok(Reasoning::with_answer("", "fork integration done"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(CaptureLLM {
                captured: Arc::clone(&captured_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(Arc::clone(&parent_messages))
    .with_system_builder(Arc::new(|_ov, _cwd| "FORK-CONTEXT-SP".to_string()));

    // Act: fork 模式触发端到端路径
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "continue from parent context"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    // Assert 0: 执行成功（v2 loop 正常完成）
    assert!(
        result.contains("fork integration done"),
        "fork should complete via v2 path: {}",
        result
    );

    let msgs = captured.lock().unwrap().clone();

    // Assert 1: 消息数量 >= 4（3 父 + 1 system + 1 fork_directive prompt）
    assert!(
        msgs.len() >= 4,
        "fork should receive parent messages + system + directive (got {})",
        msgs.len()
    );

    // Assert 2: 最后一条是 Human，且包含 <fork_directive>（BUG-A）
    let last = msgs.last().expect("messages non-empty");
    assert!(
        matches!(last, BaseMessage::Human { .. }),
        "last message should be Human (fork directive)"
    );
    let last_content = last.content();
    assert!(
        last_content.contains("<fork_directive>"),
        "last message should contain <fork_directive> (BUG-A), got: {}",
        last_content
    );
    assert!(
        last_content.contains("continue from parent context"),
        "fork directive should wrap original prompt"
    );

    // Assert 3: messages 中包含 System 消息，内容含 "FORK-CONTEXT-SP"（BUG-B）
    let sys_msg = msgs
        .iter()
        .find(|m| matches!(m, BaseMessage::System { .. }));
    assert!(
        sys_msg.is_some(),
        "fork path should inject System message (BUG-B)"
    );
    assert!(
        sys_msg.unwrap().content().contains("FORK-CONTEXT-SP"),
        "System message should contain system_builder output (BUG-B)"
    );

    // Assert 4: 父消息按顺序透传（BUG-C）
    // 验证三条父消息的 content 都在 LLM 收到的 messages 中
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert!(
        contents.iter().any(|c| c.contains("parent Q1")),
        "first parent message should pass through (BUG-C)"
    );
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent A1 with details")),
        "second parent message should pass through (BUG-C)"
    );
    assert!(
        contents.iter().any(|c| c.contains("parent Q2 followup")),
        "third parent message should pass through (BUG-C)"
    );
}

#[tokio::test]
async fn test_fork_prefers_tool_context_messages_over_parent_snapshot() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("old before_agent snapshot"));
    let ctx_messages = vec![
        BaseMessage::human("old before_agent snapshot"),
        BaseMessage::ai("new current turn detail"),
        BaseMessage::human("latest user request"),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct CaptureContentLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CaptureContentLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "ctx-preferred"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(CaptureContentLLM {
                captured: Arc::clone(&captured_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(Arc::clone(&parent_messages));

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review current turn"
            }),
            peri_agent::tools::ToolContext::new(&ctx_messages, "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("ctx-preferred"),
        "fork should execute via context-preferred path: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("new current turn detail")),
        "fork should inherit current ToolContext messages, got: {:?}",
        contents
    );
    assert!(
        contents.iter().any(|c| c.contains("latest user request")),
        "fork should inherit latest ToolContext user request, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_falls_back_to_parent_messages_when_tool_context_empty() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent fallback context"));

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct FallbackCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for FallbackCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "fallback-used"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(FallbackCaptureLLM {
                captured: Arc::clone(&captured_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(Arc::clone(&parent_messages));

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review fallback"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("fallback-used"),
        "fork should execute via fallback path: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent fallback context")),
        "fork should fall back to parent_messages when ToolContext is empty, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_drops_trailing_tool_call_message_from_tool_context() {
    let ctx_messages = vec![
        BaseMessage::human("stable context before tool call"),
        BaseMessage::ai_with_tool_calls(
            "unfinished agent tool call text",
            vec![peri_agent::messages::ToolCallRequest::new(
                "call-agent-1",
                "Agent",
                serde_json::json!({"fork": true, "prompt": "review"}),
            )],
        ),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct DropToolCallCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for DropToolCallCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "tool-call-dropped"))
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(DropToolCallCaptureLLM {
                captured: Arc::clone(&captured_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review without dangling tool call"
            }),
            peri_agent::tools::ToolContext::new(&ctx_messages, "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("tool-call-dropped"),
        "fork should execute after dropping trailing tool call message: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("stable context before tool call")),
        "fork should keep earlier context, got: {:?}",
        contents
    );
    assert!(
        contents
            .iter()
            .all(|c| !c.contains("unfinished agent tool call text")),
        "fork should drop trailing AI message with unclosed tool call, got: {:?}",
        contents
    );
}

/// 场景 2（Background Independent cancel）：端到端验证 background fork 在父 cancel 后**不**中断。
///
/// 基于 `SubAgentTool::invoke` → `invoke_background` → `invoke_background_fork`
/// → `spawn_background_fork` 完整链路（v2 `build_v2_subagent_context` 装配）。
///
/// 关键断言：
/// - 父 cancel_token.cancel() 后，background task 仍能完成（Independent policy）
/// - mock LLM 至少被调用 1 次（证明未被父 cancel 中断）
/// - bg_event_sender 接收到 BackgroundTaskCompleted（task 正常结束）
#[tokio::test]
async fn test_integration_background_independent_survives_parent_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    // Arrange: 共享的 LLM 调用计数（mock 会阻塞等待，但 cancel 不影响它）
    let llm_call_count: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_call_count_clone = Arc::clone(&llm_call_count);

    struct CountingLLM {
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CountingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "bg done independent"))
        }
    }

    // 父 cancel token（Independent policy 下不应传播到 background task）
    let parent_cancel = AgentCancellationToken::new();

    // bg_event_sender 通道，捕获完成事件
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();

    // Background registry
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    // 父消息（fork background 需要）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent ctx"));

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(CountingLLM {
                count: Arc::clone(&llm_call_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_cancel(parent_cancel.clone())
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx);

    // Act 1: 启动 background fork
    let invoke_result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "long running bg task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        invoke_result.is_ok(),
        "background fork should start: {:?}",
        invoke_result.err()
    );
    let invoke_msg = invoke_result.unwrap();
    assert!(
        invoke_msg.contains("Background task"),
        "should return background task started message: {}",
        invoke_msg
    );

    // Act 2: 父 cancel（不应影响 independent background task）
    // 注意：Independent policy = background task 使用独立 CancellationToken（spawner.rs:177），
    // 不与 parent_cancel 形成 child_token 关系，所以 cancel 不会传播。
    parent_cancel.cancel();

    // Assert 1: background task 仍在运行（active_count >= 1，未被父 cancel 移除）
    assert!(
        registry.active_count() >= 1,
        "independent background task should survive parent cancel"
    );

    // Act 3: 等待 background task 完整执行（消耗所有事件直到 BackgroundTaskCompleted）
    // Independent policy 下 task 应运行到完成，不应被 cancel 中断。
    let mut got_started = false;
    let mut got_stopped = false;
    let mut got_completed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ev)) => match ev {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    got_started = true;
                    assert!(
                        is_background,
                        "SubagentStarted should have is_background=true"
                    );
                }
                ExecutorEvent::SubagentStopped { .. } => {
                    got_stopped = true;
                }
                ExecutorEvent::BackgroundTaskCompleted(ref res) => {
                    assert!(
                        res.success,
                        "background task result should be success (independent of parent cancel)"
                    );
                    assert!(
                        res.output.contains("bg done independent"),
                        "background output should match mock LLM answer: {}",
                        res.output
                    );
                    got_completed = true;
                    break;
                }
                _ => {}
            },
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout
        }
    }

    // Assert 2: 接收到完整事件序列（Started → Stopped → Completed）
    assert!(
        got_started,
        "should receive SubagentStarted event from bg pump"
    );
    assert!(
        got_stopped,
        "should receive SubagentStopped event from bg pump"
    );
    assert!(
        got_completed,
        "should receive BackgroundTaskCompleted within timeout (independent of parent cancel)"
    );

    // Assert 3: LLM 被调用过（>=1 次），证明执行未被 cancel 中断
    let call_count = llm_call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        call_count >= 1,
        "mock LLM should be called at least once (parent cancel should not interrupt independent task), got {}",
        call_count
    );
}

/// 场景 3（Sync Cascade cancel）—— 补充断言。
///
/// 已有测试 `test_cancel_token_interrupts_subagent`（:655）验证了"父 cancel 在 SubAgent 执行前触发"
/// 的场景，但它的断言仅检查返回值 `contains("interrupted")`。这里补充验证：
/// - cancel 在执行前触发 → run_react_loop 返回 LoopResult::Interrupted
/// - 返回的 "interrupted" 字符串（通过 execute_fork.rs:236 / define.rs:584 的 output_summary）
/// - 不会调用任何工具/LLM 多次（避免 cancel 后的 zombie 执行）
///
/// 原测试 :655 已覆盖核心断言，本测试作为补充，验证 cancel 的"前置触发"边界条件
/// 与"返回值规范化"约定。
#[tokio::test]
async fn test_integration_sync_cascade_cancel_returns_interrupted_marker() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("cancellable.md"),
        "---\nname: cancellable\ndescription: Can be cancelled\n---\n\nYou are cancellable.\n",
    )
    .unwrap();

    // LLM 永远尝试调用不存在的工具，模拟"无限循环"——但 cancel 在执行前触发
    let llm_call_count: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_call_count_clone = Arc::clone(&llm_call_count);

    struct LoopingLLM {
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for LoopingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    let cancel = AgentCancellationToken::new();
    // 关键：在 SubAgent 执行**之前** cancel（模拟父 Agent 收到 Ctrl+C 后才 spawn SubAgent）
    cancel.cancel();

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(LoopingLLM {
                count: Arc::clone(&llm_call_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_cancel(cancel);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "cancellable",
                "prompt": "run",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    // Assert 1: 返回值包含 interrupted 标记（Cascade cancel 传播成功）
    assert!(
        result.contains("interrupted"),
        "Cascade cancel should produce 'interrupted' marker, got: {}",
        result
    );

    // Assert 2: cancel 在 loop 入口前阻断 LLM。
    let final_count = llm_call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        final_count, 0,
        "pre-cancelled Cascade child must not call the LLM (got {} calls)",
        final_count
    );
}

/// P0-2：background non-fork 必须通过实际 `invoke_background` 路径，由 loop 在
/// Receive 后唯一执行 before_agent。测试使用 registry 和 bg event sender，不轮询或 sleep。
#[tokio::test]
async fn test_p0_2_background_defined_skill_preload_once_after_parent_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("p0-2-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("p0-2-bg.md"),
        "---\nname: p0-2-bg\ndescription: P0-2 background agent\nskills:\n  - p0-2-skill\n---\n\nRun the task.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: p0-2-skill\ndescription: P0-2 skill\n---\n\nP0-2 BACKGROUND SKILL MARKER\n",
    )
    .unwrap();

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let preload_count = Arc::new(std::sync::Mutex::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let preload_count_clone = Arc::clone(&preload_count);
    struct BackgroundSkillLLM {
        calls: Arc<AtomicUsize>,
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BackgroundSkillLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                messages
                    .iter()
                    .any(|message| message.content().contains("p0-2 background prompt")),
                "before_agent must observe the prompt only after Receive"
            );
            *self.preload_count.lock().unwrap() = messages
                .iter()
                .filter(|message| message.content().contains("P0-2 BACKGROUND SKILL MARKER"))
                .count();
            Ok(Reasoning::with_answer("", "p0-2 background done"))
        }
    }

    let parent_cancel = AgentCancellationToken::new();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(BackgroundSkillLLM {
                calls: Arc::clone(&llm_calls_clone),
                preload_count: Arc::clone(&preload_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_cancel(parent_cancel.clone())
    .with_task_manager(registry)
    .with_bg_event_sender(bg_tx);

    let started = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "p0-2-bg",
                "run_in_background": true,
                "prompt": "p0-2 background prompt",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("background defined subagent should start");
    assert!(started.contains("Background task"));
    parent_cancel.cancel();

    let mut lifecycle = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = bg_rx.recv().await {
            match event {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    assert!(is_background);
                    lifecycle.push("started");
                }
                ExecutorEvent::SubagentStopped { .. } => lifecycle.push("stopped"),
                ExecutorEvent::BackgroundTaskCompleted(result) => {
                    assert!(result.success);
                    assert!(result.output.contains("p0-2 background done"));
                    lifecycle.push("completed");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("background defined subagent must complete within the bounded receiver timeout");

    assert_eq!(lifecycle, ["started", "stopped", "completed"]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *preload_count.lock().unwrap(),
        1,
        "explicit skill preload must occur exactly once on execute_bg.rs"
    );
}

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

    struct PromptCaptureLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for PromptCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            // 找到最后一条 Human 消息（fork directive 在 prompt queue 里）
            if let Some(last_human) = messages
                .iter()
                .rev()
                .find(|m| matches!(m, BaseMessage::Human { .. }))
            {
                *self.captured.lock().unwrap() = last_human.content();
            }
            Ok(Reasoning::with_answer("", "bg-fork done"))
        }
    }

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("ctx for bg fork"));

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(PromptCaptureLLM {
                captured: Arc::clone(&prompt_capture_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx);

    // Act: 同时 fork=true + run_in_background=true（优先级测试）
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "do both"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
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
    match started_ev {
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

// ─── C2/C3：v2 SubagentStart/Stop 生产 emit 契约测试 ─────────────────────────
//
// 每条生产路径（fork 同步 / define 同步 / bg 非 fork / bg fork）各一测试：
// Start/Stop 恰好一次、字段配对、child_agent_id 为 UUID v7（= child_thread_id）。
// 捕获通道：child EventBus → forwarder observe 分支 → mock LangfuseBridgeLike
// （v1 mapper 转发已被过滤，见 peri-agent subagent_event_forwarder 测试）。

/// mock LangfuseBridgeLike：记录 forwarder 转发的全部 ObserveEvent
struct RecordingBridge {
    observes: Arc<std::sync::Mutex<Vec<ObserveEvent>>>,
}

impl peri_agent::agent::LangfuseBridgeLike for RecordingBridge {
    fn process_render_event(&self, _ev: &peri_agent::agent::events_v2::RenderEvent) {}

    fn process_observe_event(&self, ev: &ObserveEvent) {
        self.observes.lock().unwrap().push(ev.clone());
    }
}

/// 构造注入父身份的 SubAgentTool + 记录 bridge（parent_agent_id 已 set → emit 生效）
fn make_tool_with_bridge() -> (SubAgentTool, Arc<RecordingBridge>) {
    let bridge = Arc::new(RecordingBridge {
        observes: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
    .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
    .with_langfuse_bridge(Arc::clone(&bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>);
    (t, bridge)
}

/// 轮询等待 bridge 收到 Start 与 Stop 各至少一次（forwarder 异步消费，
/// 内容事件可能先到，不能只按数量等待）
async fn wait_for_observe_start_stop(
    bridge: &Arc<RecordingBridge>,
    timeout_ms: u64,
) -> Vec<ObserveEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let evs = bridge.observes.lock().unwrap().clone();
        let has_start = evs
            .iter()
            .any(|e| matches!(e, ObserveEvent::SubagentStart { .. }));
        let has_stop = evs
            .iter()
            .any(|e| matches!(e, ObserveEvent::SubagentStop { .. }));
        if has_start && has_stop {
            return evs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 v2 SubagentStart/Stop 超时（{}ms）：当前事件：{:?}",
                timeout_ms, evs
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// 断言 Start/Stop 恰好一次且字段配对（agent_name / is_background / 父子 id 一致）
fn assert_start_stop_pair(evs: &[ObserveEvent], expected_name: &str, expected_bg: bool) {
    let starts: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
        .collect();
    let stops: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
        .collect();
    assert_eq!(starts.len(), 1, "SubagentStart 必须恰好一次: {:?}", evs);
    assert_eq!(stops.len(), 1, "SubagentStop 必须恰好一次: {:?}", evs);

    let (start_parent, start_child, start_name, start_bg) = match starts[0] {
        ObserveEvent::SubagentStart {
            agent_id,
            child_agent_id,
            agent_name,
            is_background,
            ..
        } => (agent_id, child_agent_id, agent_name, is_background),
        _ => unreachable!(),
    };
    let (stop_parent, stop_child, stop_name, stop_result, stop_err) = match stops[0] {
        ObserveEvent::SubagentStop {
            agent_id,
            child_agent_id,
            agent_name,
            result,
            is_error,
            ..
        } => (agent_id, child_agent_id, agent_name, result, is_error),
        _ => unreachable!(),
    };
    assert_eq!(start_name.as_str(), expected_name, "agent_name 不符");
    assert_eq!(*start_bg, expected_bg, "is_background 不符");
    assert_eq!(
        start_parent, stop_parent,
        "Start/Stop 父 agent_id 必须一致（同一次调用）"
    );
    assert_eq!(
        start_child, stop_child,
        "Start/Stop child_agent_id 必须配对（同一 subagent）"
    );
    assert_eq!(stop_name.as_str(), expected_name, "Stop agent_name 不符");
    assert!(!stop_result.is_empty() || *stop_err, "Stop 必须携带 result");
    assert!(
        uuid::Uuid::parse_str(&start_child.to_string()).is_ok(),
        "child_agent_id 必须是可解析 UUID（= child_thread_id）"
    );
}

fn write_test_agent(dir: &tempfile::TempDir) {
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();
}

/// 从事件流中取出 Start 事件的 child_agent_id
fn start_child_agent_id(evs: &[ObserveEvent]) -> peri_acp_types::identity::AgentId {
    evs.iter()
        .find_map(|e| match e {
            ObserveEvent::SubagentStart { child_agent_id, .. } => Some(*child_agent_id),
            _ => None,
        })
        .expect("事件流中应有 SubagentStart")
}

/// S1/T1：fork 同步路径（execute_fork.rs）—— Start/Stop 恰好一次，
/// 且 child_agent_id == child_thread_id（C1 身份统一契约）
#[tokio::test]
async fn test_fork_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    // thread_store 存在时 invoke 返回携带 child_thread_id，用于身份对齐断言
    let (t, bridge) = make_tool_with_bridge();
    let t = t.with_thread_store(Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    )) as Arc<dyn peri_agent::thread::ThreadStore>);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "fork task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "fork 应成功: {:?}", result.err());
    let result = result.unwrap();
    let child_thread_id = result
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("fork 返回值应包含 child_thread_id")
        .to_string();

    let evs = wait_for_observe_start_stop(&bridge, 3000).await;
    assert_start_stop_pair(&evs, "fork", false);
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        child_thread_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S4/T4：define 同步路径（define.rs）—— Start/Stop 恰好一次，
/// 且 child_agent_id == child_thread_id（C1 身份统一契约）
#[tokio::test]
async fn test_define_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let (t, bridge) = make_tool_with_bridge();
    let t = t.with_thread_store(Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    )) as Arc<dyn peri_agent::thread::ThreadStore>);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "do it"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "define 应成功: {:?}", result.err());
    let result = result.unwrap();
    let child_thread_id = result
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("define 返回值应包含 child_thread_id")
        .to_string();

    let evs = wait_for_observe_start_stop(&bridge, 3000).await;
    assert_start_stop_pair(&evs, "test-agent", false);
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        child_thread_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S2/T2：bg 非 fork 路径（execute_bg.rs）—— Start/Stop 恰好一次（is_background=true），
/// 且 child_agent_id == v1 SubagentStarted.instance_id（C1 身份统一契约）
#[tokio::test]
async fn test_background_path_emits_v2_start_stop_exactly_once() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (t, bridge) = make_tool_with_bridge();
    let t = t
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "bg task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "bg 应启动成功: {:?}", result.err());

    let evs = wait_for_observe_start_stop(&bridge, 5000).await;
    assert_start_stop_pair(&evs, "test-agent", true);

    // v1 SubagentStarted.instance_id（= child_thread_id）与 v2 Start.child_agent_id 对齐
    let instance_id = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("应收到 SubagentStarted")
        .expect("通道不应关闭");
    let instance_id = match instance_id {
        ExecutorEvent::SubagentStarted { instance_id, .. } => instance_id,
        other => panic!("应为 SubagentStarted，实际 {:?}", other),
    };
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        instance_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// S3/T3：bg fork 路径（spawner.rs spawn_background_fork）—— Start/Stop 恰好一次，
/// 且 child_agent_id == v1 SubagentStarted.instance_id（C1 身份统一契约）
#[tokio::test]
async fn test_bg_fork_path_emits_v2_start_stop_exactly_once() {
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> =
        Arc::new(RwLock::new(vec![BaseMessage::human("ctx for bg fork")]));
    let (t, bridge) = make_tool_with_bridge();
    let t = t
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "bg fork task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "bg fork 应启动成功: {:?}", result.err());

    let evs = wait_for_observe_start_stop(&bridge, 5000).await;
    assert_start_stop_pair(&evs, "fork", true);

    // v1 SubagentStarted.instance_id（= child_thread_id）与 v2 Start.child_agent_id 对齐
    let instance_id = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("应收到 SubagentStarted")
        .expect("通道不应关闭");
    let instance_id = match instance_id {
        ExecutorEvent::SubagentStarted { instance_id, .. } => instance_id,
        other => panic!("应为 SubagentStarted，实际 {:?}", other),
    };
    assert_eq!(
        start_child_agent_id(&evs).to_string(),
        instance_id,
        "C1：Start.child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

// ─── S3.1 注册门控 + S3.2 取消收尾（issue 2026-08-05）────────────────────

/// 构造一个已注册状态的 bg 任务（预置 registry 占用额度用）
fn make_registered_bg_task(id: &str) -> peri_agent::agent::async_tasks::BackgroundTask {
    use peri_agent::agent::async_tasks::{
        BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
    };
    let handle = tokio::runtime::Handle::current().spawn(async {});
    BackgroundTask {
        id: id.to_string(),
        agent_name: "pre-seeded".to_string(),
        prompt_summary: "pre-seeded task".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: None,
        pid: None,
        output_preview: None,
    }
}

/// [回归测试] S3.1 幽灵任务：注册失败（并发撞 kind 上限）的任务必须不执行。
///
/// 预检（total ≥ 3）与注册（per-kind 上限）之间的竞态无法单测自然触发，
/// 用 barrier 确定性制造：预置 2 个 Agent 任务（total=2），4 个并发 invoke 都
/// 通过 total 预检后同步汇合在 llm_factory，放行后串行注册——agent kind 上限 3
/// 只容 1 个成功，其余 3 个必须：
/// - invoke 返回 "Failed to register" 错误（如实）
/// - 不执行 run_react_loop（零 LLM 调用）
/// - 不 emit 任何事件（无 SubagentStarted → 无配对问题）
/// - 不注册 register_runtime（无需 deregister）
///
/// 历史 bug（issue 2026-08-05）：注册失败仅 return Err，任务已 spawn 继续跑，
/// 幽灵执行 + double 泄漏（register_runtime 无配对 deregister）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_bg_register_failure_does_not_execute_task() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("gate-agent.md"),
        "---\nname: gate-agent\ndescription: Gate test\n---\n\nYou are gated.\n",
    )
    .unwrap();

    // 预置 2 个 Agent 任务（total=2）：4 个并发 invoke 都能通过 total 预检
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    for i in 0..2 {
        registry
            .register_with_kind(make_registered_bg_task(&format!("bg-pre-{}", i)))
            .unwrap();
    }
    assert_eq!(registry.active_count(), 2);

    // barrier：4 个 invoke 都通过预检并到达 llm_factory 后放行（确定性竞态窗口）
    let gate = Arc::new(Barrier::new(4));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);
    // 成功注册的任务阻塞在 LLM 调用（保持 kind 额度占用，防止任务快速完成
    // 触发 complete 移除条目后额度回落、后续注册"假成功"）
    let llm_gate = Arc::new(tokio::sync::Notify::new());
    let llm_gate_clone = Arc::clone(&llm_gate);

    struct GateLLM {
        calls: Arc<AtomicUsize>,
        block: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for GateLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞成功任务：注册窗口内 kind 额度不释放
            self.block.notified().await;
            Ok(Reasoning::with_answer("", "bg gate done"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            // 4 个 invoke 在此同步汇合（保证全部通过预检后才放行注册）
            gate_clone.wait();
            Box::new(GateLLM {
                calls: Arc::clone(&llm_calls_clone),
                block: Arc::clone(&llm_gate_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    // register_runtime / deregister_runtime mock：记录调用
    let registered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let registered_clone = registered.clone();
    let deregistered_clone = deregistered.clone();
    let register_cb: Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync> =
        Arc::new(move |tid, _tok, _pol| {
            registered_clone.lock().unwrap().push(tid);
        });
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx)
    .with_register_runtime(register_cb)
    .with_deregister_runtime(deregister_cb);

    // 4 个并发 invoke——必须各自 tokio::spawn（llm_factory 内的 Barrier::wait()
    // 是同步阻塞：若在 join_all 单任务内逐个 poll，第一个 future 会卡死当前
    // worker，其余 3 个永远不被 poll，barrier 凑不齐 4 个参与者而死锁）。
    let tool = Arc::new(tool);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let tool = Arc::clone(&tool);
        let cwd = dir.path().to_str().unwrap().to_string();
        handles.push(tokio::spawn(async move {
            tool.invoke(
                serde_json::json!({
                    "subagent_type": "gate-agent",
                    "run_in_background": true,
                    "prompt": "parallel bg task",
                    "cwd": cwd,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
        }));
    }
    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("invoke 任务不应 panic"))
        .collect();

    // 恰好 1 个注册成功，3 个注册失败（错误信息如实返回）
    let oks = results.iter().filter(|r| r.is_ok()).count();
    let errs = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 1, "恰好 1 个并发任务注册成功，实际 {}", oks);
    assert_eq!(errs, 3, "其余 3 个必须注册失败，实际 {}", errs);
    for r in &results {
        if let Err(e) = r {
            assert!(
                e.to_string().contains("Failed to register"),
                "注册失败错误应如实返回: {}",
                e
            );
        }
    }

    // 等待成功任务 emit SubagentStarted（只有注册成功的任务实际执行）
    let mut started = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => {
                started += 1;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        started, 1,
        "只有注册成功的任务 emit SubagentStarted，实际 {}",
        started
    );

    // 成功任务阻塞在 LLM 调用：等待小窗口后断言无任何完成事件（失败任务零事件）
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut stopped = 0usize;
    let mut completed = 0usize;
    while let Ok(ev) = bg_rx.try_recv() {
        match ev {
            ExecutorEvent::SubagentStopped { .. } => stopped += 1,
            ExecutorEvent::BackgroundTaskCompleted(_) => completed += 1,
            _ => {}
        }
    }
    assert_eq!(
        stopped, 0,
        "注册失败的任务不得 emit SubagentStopped（无幽灵完成）"
    );
    assert_eq!(completed, 0, "注册失败的任务不得产生完成事件（无幽灵完成）");
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        1,
        "注册失败的任务不得执行 run_react_loop（LLM 仅被成功任务调用一次），实际 {}",
        llm_calls.load(Ordering::SeqCst)
    );
    // register_runtime 只在注册成功后执行（失败任务零注册 → 无需 deregister）
    assert_eq!(
        registered.lock().unwrap().len(),
        1,
        "仅注册成功的任务进入 active_agents"
    );
    assert_eq!(
        deregistered.lock().unwrap().len(),
        0,
        "任务仍在运行（阻塞），不得提前 deregister"
    );
    // registry 无幽灵条目：2 预置 + 1 成功注册（任务阻塞未完成）→ 3
    assert_eq!(registry.active_count(), 3, "registry 不应有幽灵条目");
}

/// [回归测试] S3.2 取消收尾：cancel() 先 token.cancel()，任务响应取消链走
/// 完整收尾——SubagentStopped 配对（subagent_depth 归零）、active_agents
/// deregister（任务内同步 guard）、registry 层无幽灵 Completed 事件。
///
/// 历史 bug（issue 2026-08-05）：取消仅 abort，收尾全部跳过（active_agents
/// 泄漏 + depth 错乱 + thread 状态停留 running）。
#[tokio::test]
async fn test_bg_cancel_trigger_token_and_cleanup() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("blocking-agent.md"),
        "---\nname: blocking-agent\ndescription: Blocks\n---\n\nYou block.\n",
    )
    .unwrap();

    // LLM 在 generate_reasoning 中阻塞（模拟长时间运行的 bg agent；
    // reason 阶段的 biased select 会在 cancel 后 drop 本 future 并返回 Interrupted）
    let gate = Arc::new(tokio::sync::Notify::new());
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);

    struct BlockingLLM {
        gate: Arc<tokio::sync::Notify>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BlockingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞直到被取消（select 放弃本 future）
            self.gate.notified().await;
            Ok(Reasoning::with_answer("", "never"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            Box::new(BlockingLLM {
                gate: Arc::clone(&gate_clone),
                calls: Arc::clone(&llm_calls_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let (reg_events_tx, mut reg_events_rx) =
        mpsc::unbounded_channel::<peri_agent::agent::async_tasks::BgRegistryEvent>();
    registry.set_event_sender(reg_events_tx, "sess-cancel".to_string());

    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered_clone = deregistered.clone();
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx)
    .with_deregister_runtime(deregister_cb);

    let msg = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "blocking-agent",
                "run_in_background": true,
                "prompt": "block forever",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("bg task should start");
    assert!(msg.contains("Background task"));

    // 等待 LLM 进入阻塞（任务真正运行中，位于 reason 的 select 内）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while llm_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("LLM 应被调用（任务运行中）");

    // 取消：token.cancel() 应让任务响应并走完整收尾
    let tasks = registry.list_tasks();
    let (task_id, _, _) = tasks.into_iter().next().expect("任务应已注册");
    registry.cancel(&task_id).unwrap();
    assert_eq!(registry.active_count(), 0, "取消后条目已移除");

    // 事件流：SubagentStopped 必须到达（与 SubagentStarted 配对，depth 归零）
    let mut started = 0usize;
    let mut stopped = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => started += 1,
            Ok(Some(ExecutorEvent::SubagentStopped { .. })) => stopped += 1,
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(res))) => {
                assert!(!res.success, "取消后任务结果应为失败（interrupted）");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(started, 1);
    assert_eq!(
        stopped, 1,
        "取消后任务应 emit SubagentStopped（与 Started 配对）"
    );

    // active_agents 注销（任务内同步收尾 guard）：complete 后闭包结束触发 drop
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while deregistered.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("取消后任务收尾应 deregister active_agents");
    assert_eq!(deregistered.lock().unwrap().len(), 1);

    // registry 层无幽灵 Completed 事件（complete 对已移除条目返回 false 不推事件）
    let mut saw_completed = false;
    while let Ok(ev) = reg_events_rx.try_recv() {
        if matches!(
            ev,
            peri_agent::agent::async_tasks::BgRegistryEvent::Completed { .. }
        ) {
            saw_completed = true;
        }
    }
    assert!(!saw_completed, "取消后不得推幽灵 Completed 事件");
}

// ─── Slice 6:Agent 工具 resume_thread_id 参数（tool 层） ──────────────────────

/// 构造 FilesystemThreadStore（写盘即时刷新，无需 flush）
fn make_fs_store(dir: &tempfile::TempDir) -> Arc<peri_agent::thread::FilesystemThreadStore> {
    Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    ))
}

/// 预置可恢复 thread：创建（title 决定工具集恢复路径）+ 写消息 + 置非 active。
/// FilesystemThreadStore 写盘即时落库（append 后 load_messages 立即可见）。
async fn preset_resumable_thread(
    store: &Arc<peri_agent::thread::FilesystemThreadStore>,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    let id = id.to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new("/tmp/work");
    meta.id = id.clone();
    meta.title = Some(title.to_string());
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    meta.hidden = true;
    store.create_thread(meta).await.unwrap();
    if !msgs.is_empty() {
        store.append_messages(&id, &msgs).await.unwrap();
    }
    store.update_thread_status(&id, "done").await.unwrap();
}

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

/// 校验：parent 链不匹配（meta.parent_thread_id ≠ 父 session thread_id）→ Err
#[tokio::test]
async fn test_resume_thread_id_parent_mismatch() {
    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, "fork", Some("some-other-parent"), Vec::new()).await;

    // 父 session：store().thread_id = "parent-uuid" ≠ meta.parent_thread_id
    let parent = peri_agent::session::Session::new(
        Arc::from("/tmp/work"),
        peri_agent::session::FrozenContext::builder().build(),
        Some("parent-uuid".into()),
    );
    let t = make_subagent_tool(vec![])
        .with_thread_store(store)
        .with_parent_session(parent);
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
        err.contains("parent thread mismatch"),
        "parent 链不匹配应被拒绝: {}",
        err
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

// ─── Slice 7:集成测试(中断 → 恢复 → 完成 / 跨实例 / 多次恢复 / 事件配对) ─────

/// 前 `interrupt_rounds` 次 LLM 调用返回 `AgentError::Interrupted`（模拟中断），
/// 之后回显最后一条消息（模拟正常完成）。共享计数跨 tool 实例 / 跨恢复生效
/// ——每次 subagent 执行都会经 llm_factory 创建新实例，计数保持连续。
struct InterruptThenEchoLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    interrupt_rounds: usize,
}

#[async_trait::async_trait]
impl ReactLLM for InterruptThenEchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < self.interrupt_rounds {
            return Err(peri_agent::error::AgentError::Interrupted);
        }
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

/// 构造带「前 N 次 Interrupted、之后回显」LLM 的 SubAgentTool（无 bridge/parent）
fn make_interrupt_tool(
    calls: Arc<std::sync::atomic::AtomicUsize>,
    interrupt_rounds: usize,
) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(InterruptThenEchoLLM {
                calls: Arc::clone(&calls),
                interrupt_rounds,
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
}

/// 从工具返回/错误文本提取 child_thread_id
fn extract_child_thread_id(text: &str) -> String {
    text.split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("文本应包含 child_thread_id")
        .to_string()
}

/// 统计 transcript 中 SkillTool 工具调用消息数（SkillPreload 注入的
/// Ai[ToolUse{SkillTool}] block 计数；R-H1 断言用）
fn count_skilltool_calls(msgs: &[BaseMessage]) -> usize {
    msgs.iter()
        .flat_map(|m| m.content_blocks())
        .filter(|b| {
            matches!(
                b,
                peri_agent::messages::ContentBlock::ToolUse { name, .. }
                    if name.as_str() == "SkillTool"
            )
        })
        .count()
}

/// 轮询等待 bridge 收到至少 n 对 Start/Stop（forwarder 异步消费，
/// 内容事件可能先到，不能只按数量等待）
async fn wait_for_observe_pairs(
    bridge: &Arc<RecordingBridge>,
    n: usize,
    timeout_ms: u64,
) -> Vec<ObserveEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let evs = bridge.observes.lock().unwrap().clone();
        let starts = evs
            .iter()
            .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
            .count();
        let stops = evs
            .iter()
            .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
            .count();
        if starts >= n && stops >= n {
            return evs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 {} 对 SubagentStart/Stop 超时（{}ms）：当前事件：{:?}",
                n, timeout_ms, evs
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// 轮询等待 transcript 异步 writer 落盘（transcript.rs 批量窗口 ≤100ms；
/// 执行返回后新消息可能仍在 writer 通道中）
async fn wait_for_messages(
    store: &Arc<peri_agent::thread::FilesystemThreadStore>,
    id: &str,
    n: usize,
    timeout_ms: u64,
) -> Vec<BaseMessage> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let msgs = store.load_messages(&id.to_string()).await.unwrap();
        if msgs.len() >= n {
            return msgs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 transcript 落盘超时（{}ms）：期望 >= {} 条，实际 {} 条",
                timeout_ms,
                n,
                msgs.len()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 核心链路：agent-def 路径调用 Agent 工具 → LLM 返回 Interrupted（错误文本
/// 带 child_thread_id 前缀，主 agent 凭此恢复）→ 新 tool 实例（同一 dir /
/// 同 store / 同父 session thread_id，模拟主 agent 下一 turn 或进程重启）
/// 调用 resume_thread_id → 完成文本含结果。
/// R-L3：进程重启复用相同父 thread_id → parent 校验通过。
#[tokio::test]
async fn test_resume_interrupted_then_resumed_across_instances() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("interrupt-agent.md"),
        "---\nname: interrupt-agent\ndescription: Interrupt test agent\n---\n\nYou get interrupted.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    // 主 agent 会话 thread_id 固定（进程重启后 session_id 不变 → 父链校验跨进程成立，R-L3）
    let parent = peri_agent::session::Session::new(
        Arc::from("/tmp/work"),
        peri_agent::session::FrozenContext::builder().build(),
        Some("parent-uuid".into()),
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // 实例 A：spawn → LLM 首轮 Interrupted → Err 文本带 child_thread_id 前缀
    let t_a = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_parent_session(parent.clone());
    let err = t_a
        .invoke(
            serde_json::json!({
                "subagent_type": "interrupt-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "first task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "LLM 返回 Interrupted → 错误文本须带 child_thread_id 前缀（可恢复）: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // 实例 B（同 store dir、同父 session thread_id）：resume → 完成
    let t_b = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_parent_session(parent);
    let result = t_b
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("resume 应成功完成（parent 校验通过）");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    assert!(
        result.contains("echo:"),
        "完成文本应含执行结果（隐式 continue 后的回显）: {}",
        result
    );

    // R-L3：子线程父链 = 父 session thread_id（实例 B 复用同一父 id → parent 校验通过）
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(
        meta.parent_thread_id.as_deref(),
        Some("parent-uuid"),
        "子线程父链必须指向父 session thread_id（跨实例复用）"
    );
}

/// 跨实例重载（进程重启）：实例 A spawn 并中断（LLM 首轮返回 Interrupted →
/// 错误文本，带 child_thread_id 前缀）→ 丢弃实例 A → 新建实例 B（同
/// FilesystemThreadStore dir）resume → 完成；断言 transcript 重放正确
/// （消息数/顺序：spawn prompt → 隐式 continue → 新 AI）。
#[tokio::test]
async fn test_resume_across_instances_replays_transcript_in_order() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let id = {
        // 实例 A：spawn → LLM 首轮 Interrupted（thread 与 transcript 已落盘）
        let t_a = make_interrupt_tool(Arc::clone(&calls), 1)
            .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
        let err = t_a
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "cwd": dir.path().to_str().unwrap(),
                    "prompt": "first task"
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .expect_err("首次执行应返回（Interrupted 错误）");
        let err = err.to_string();
        assert!(
            err.contains("execution failed") && err.contains("child_thread_id:"),
            "LLM 返回 Interrupted → 错误文本须带 child_thread_id 前缀: {}",
            err
        );
        extract_child_thread_id(&err)
    }; // 丢弃实例 A（模拟进程重启，仅剩磁盘现场）

    // 实例 B：同 store dir → resume（缺省 prompt → 隐式 continue）→ 完成
    let t_b = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let result = t_b
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // transcript 重放：旧消息（spawn prompt）→ 隐式 continue → 新 AI（顺序不变）
    let msgs = wait_for_messages(&store, &id, 3, 3000).await;
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert_eq!(
        contents,
        vec![
            "first task",
            "Continue your previous task where you left off.",
            "echo: Continue your previous task where you left off.",
        ],
        "transcript 必须按 旧消息 → continue → 新 AI 顺序重放"
    );
}

/// 多次恢复：中断 → 恢复 → 再中断 → 再恢复（cancel 前置 → Ok 中断文本，
/// 含 `resume with Agent(resume_thread_id:)` 提示）；断言 thread_id 不变、
/// 最终完成、磁盘 status 收尾 done。
#[tokio::test]
async fn test_resume_multiple_times_keeps_thread_id_and_completes() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 构造带「已取消 cancel token」的实例（parent 缺席时注入的 cancel 生效 →
    // run_react_loop 返回 LoopResult::Interrupted → Ok 中断文本）
    let mk_cancelled = || {
        let cancel = AgentCancellationToken::new();
        cancel.cancel();
        make_subagent_tool(vec![])
            .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
            .with_cancel(cancel)
    };

    // 1) spawn → 中断 #1（文本含 child_thread_id + resume 提示）
    let t1 = mk_cancelled();
    let r1 = t1
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": cwd.clone(),
                "prompt": "task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("第一次应返回（中断文本）");
    assert!(
        r1.contains("was interrupted") && r1.contains("resume with Agent(resume_thread_id:"),
        "第一次应中断且带 resume 提示: {}",
        r1
    );
    let id1 = extract_child_thread_id(&r1);

    // 2) resume → 中断 #2（同一 thread_id）
    let t2 = mk_cancelled();
    let r2 = t2
        .invoke(
            serde_json::json!({
                "resume_thread_id": id1.clone(),
                "cwd": cwd.clone(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("第二次应返回（中断文本）");
    assert!(
        r2.contains("was interrupted") && r2.contains("resume with Agent(resume_thread_id:"),
        "第二次应再次中断且带 resume 提示: {}",
        r2
    );
    let id2 = extract_child_thread_id(&r2);
    assert_eq!(id1, id2, "多次恢复 thread_id 必须不变");

    // 3) resume → 完成（无 cancel 的新实例）
    let t3 =
        make_subagent_tool(vec![]).with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let r3 = t3
        .invoke(
            serde_json::json!({
                "resume_thread_id": id1.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        r3.contains(&format!("child_thread_id: {}", id1)),
        "完成文本应带 child_thread_id: {}",
        r3
    );
    assert!(r3.contains("echo:"), "最终应完成: {}", r3);

    let meta = store.load_meta(&id1).await.unwrap();
    assert_eq!(
        meta.agent_status,
        peri_agent::thread::AgentStatus::Done,
        "多次恢复后最终 status 应为 done"
    );
}

/// Start/Stop 配对：每次执行（首次 + 每次恢复）触发新 Start/Stop 配对，
/// 配对顺序正确（Start→Stop→Start→Stop）、child_agent_id 恒同（同一 thread）。
#[tokio::test]
async fn test_resume_emits_new_start_stop_pair_per_execution() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let bridge = Arc::new(RecordingBridge {
        observes: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(InterruptThenEchoLLM {
                calls: Arc::clone(&calls_clone),
                interrupt_rounds: 1,
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
    .with_langfuse_bridge(Arc::clone(&bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>)
    .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行 → 中断（第 1 对 Start/Stop；LLM 返回 Interrupted → Err）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": cwd.clone(),
                "prompt": "task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);
    let evs = wait_for_observe_pairs(&bridge, 1, 3000).await;
    assert_start_stop_pair(&evs, "test-agent", false);

    // 恢复 → 完成（第 2 对 Start/Stop）
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);
    let evs = wait_for_observe_pairs(&bridge, 2, 5000).await;

    // 恰好 2 对；事件顺序 Start→Stop→Start→Stop；child_agent_id 恒同
    let starts: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
        .collect();
    let stops: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
        .collect();
    assert_eq!(starts.len(), 2, "每次执行必须恰好一个 Start: {:?}", evs);
    assert_eq!(stops.len(), 2, "每次执行必须恰好一个 Stop: {:?}", evs);
    let seq: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            ObserveEvent::SubagentStart { .. } => Some("S"),
            ObserveEvent::SubagentStop { .. } => Some("T"),
            _ => None,
        })
        .collect();
    assert_eq!(
        seq,
        vec!["S", "T", "S", "T"],
        "配对顺序必须为 Start→Stop→Start→Stop"
    );

    let child_of = |e: &ObserveEvent| match e {
        ObserveEvent::SubagentStart { child_agent_id, .. } => *child_agent_id,
        ObserveEvent::SubagentStop { child_agent_id, .. } => *child_agent_id,
        _ => unreachable!(),
    };
    let first_child = child_of(starts[0]);
    assert_eq!(child_of(stops[0]), first_child, "第 1 对 child 必须配对");
    assert_eq!(
        child_of(starts[1]),
        first_child,
        "恢复必须复用同一 child_agent_id"
    );
    assert_eq!(child_of(stops[1]), first_child, "第 2 对 child 必须配对");
    assert_eq!(
        first_child.to_string(),
        id,
        "child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// R-H1：agent-def 声明 skills 非空 → 首次执行注入一套 SkillTool 对 → 中断 →
/// resume（恢复路径 skill_names 恒空 → 不重复注入）→ transcript 中 SkillTool
/// 工具调用消息恒为一套。
#[tokio::test]
async fn test_resume_skill_preload_not_duplicated() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("skill-user.md"),
        "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行（agent-def 路径）→ SkillPreload 注入一套 → 中断（LLM 返回 Interrupted）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "cwd": cwd.clone(),
                "prompt": "test task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // resume（隐式 continue，无 /skill token → 自动检测分支不触发）→ 完成
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // transcript 中 SkillTool 工具调用必须只有首轮注入的一套（R-H1）
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    assert_eq!(
        count_skilltool_calls(&msgs),
        1,
        "resume 不得重复注入 agent 定义声明的 skill（transcript: {:?}）",
        msgs.iter().map(|m| m.content()).collect::<Vec<_>>()
    );
}

/// R2-MID-1：构造「已完成工具轮次（末条 = Tool 结果）后中断」的 thread →
/// resume → 已完成 Ai+Tool 对保留、无重复执行（LLM 输入不含重复工具结果；
/// transcript 仅追加 continue + 新 AI，已完成轮次原样保留）。
#[tokio::test]
async fn test_resume_keeps_completed_tool_round_no_duplicate_execution() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    // 完整工具轮次：Ai[ToolUse] + Tool[result] 配对，末条 = Tool → 不 pop
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        None,
        vec![
            BaseMessage::human("task"),
            BaseMessage::ai_from_blocks(vec![peri_agent::messages::ContentBlock::tool_use(
                "call-1",
                "Read",
                serde_json::json!({}),
            )]),
            BaseMessage::tool_result("call-1", "tool-result-1"),
        ],
    )
    .await;

    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let seen_clone = Arc::clone(&seen);
    struct ToolRoundCheckLLM {
        seen: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ToolRoundCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let n = messages
                .iter()
                .filter(|m| m.content().contains("tool-result-1"))
                .count();
            *self.seen.lock().unwrap() = n;
            Ok(Reasoning::with_answer("", "round-preserved"))
        }
    }
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ToolRoundCheckLLM {
                seen: Arc::clone(&seen_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("round-preserved"),
        "resume 应完成: {}",
        result
    );
    assert_eq!(
        *seen.lock().unwrap(),
        1,
        "已完成工具轮次的 Tool 结果不得重复出现在 LLM 输入"
    );

    // transcript：3 旧消息 + 隐式 continue + 新 AI = 5 条；Ai+Tool 对保留
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert_eq!(msgs.len(), 5, "3 旧 + continue + 新 AI: {:?}", contents);
    assert!(contents[0].contains("task"), "旧 Human 应保留");
    assert!(
        contents[2].contains("tool-result-1"),
        "旧 Tool 结果应保留（已完成轮次不重放不删除）"
    );
    assert!(
        contents[3].contains("Continue your previous task"),
        "隐式 continue 应追加"
    );
    assert!(
        contents[4].contains("round-preserved"),
        "新 AI 应追加（无重复执行）"
    );
}

/// R2-LOW-1（实际行为比计划假设更安全）：resume prompt 含 /skill-name token
/// 也**不会**再次注入——resume 链装配时 skill_names 恒空（R-H1），
/// build_subagent_middlewares 的挂载条件 `!skill_names.is_empty()` 使
/// SkillPreloadMiddleware 在恢复路径根本不注册，自动检测分支（主 Agent 路径）
/// 无从触发。锁定该行为：SkillTool 计数恒为 1（首轮声明注入的一套）。
#[tokio::test]
async fn test_resume_skill_token_in_prompt_reinjects_once() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("skill-user.md"),
        "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行（显式声明 skills）→ 注入一套 → 中断（LLM 返回 Interrupted）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "cwd": cwd.clone(),
                "prompt": "test task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // resume prompt 含 /test-skill token → 自动检测分支不存在（链未挂 SkillPreload）
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
                "prompt": "/test-skill continue",
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // 断言前等待全部落盘：3 旧 + Human(/test-skill continue) + 新 AI = 5 条
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    assert_eq!(
        count_skilltool_calls(&msgs),
        1,
        "resume 链装配不挂 SkillPreloadMiddleware（R-H1）→ resume prompt 含 \
         /skill-name 也不重复注入（R2-LOW-1 窗口不存在，比计划假设更安全）"
    );
}
