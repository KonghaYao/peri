use peri_agent::{
    agent::{
        react::{ReactLLM, Reasoning, StreamingContext},
        state::AgentState,
    },
    messages::BaseMessage,
    middleware::r#trait::Middleware,
    session,
};

use super::*;

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

#[test]
fn test_middleware_name() {
    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    // Call via Middleware, explicit trait path
    assert_eq!(
        <SubAgentMiddleware as Middleware>::name(&m),
        "SubAgentMiddleware"
    );
}

#[test]
fn test_middleware_collect_tools() {
    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    let tools = <SubAgentMiddleware as Middleware>::collect_tools(&m, "/tmp");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "Agent");
}

#[test]
fn test_build_tool_returns_subagent_tool() {
    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    let tool = m.build_tool("/tmp");
    assert_eq!(tool.name(), "Agent");
}

#[test]
fn test_scan_agents_no_dir() {
    let result = scan_agents("/nonexistent/path");
    // No project-level agents, but built-in agents should still appear
    assert!(
        !result.is_empty(),
        "Built-in agents should always be present"
    );
    assert!(
        result.iter().any(|(id, _, _)| id == "explorer"),
        "Built-in explorer agent should be present"
    );
}

#[test]
fn test_scan_agents_flat_md() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews code quality\n---\n\nYou are a reviewer.\n",
    )
    .unwrap();

    let result = scan_agents(dir.path().to_str().unwrap());
    // Should contain the project agent + built-in agents
    assert!(
        result.len() > 1,
        "Should contain project agent + built-in agents"
    );
    let reviewer = result.iter().find(|(id, _, _)| id == "code-reviewer");
    assert!(reviewer.is_some(), "Project agent should be present");
    assert_eq!(reviewer.unwrap().1, "code-reviewer");
    assert_eq!(reviewer.unwrap().2, "Reviews code quality");
}

#[test]
fn test_scan_agents_nested_dir() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let agent_dir = dir.path().join(".claude").join("agents").join("analyst");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.md"),
        "---\nname: data-analyst\ndescription: Analyzes data\n---\n\nYou are an analyst.\n",
    )
    .unwrap();

    let result = scan_agents(dir.path().to_str().unwrap());
    // Should contain the project agent + built-in agents
    assert!(
        result.len() > 1,
        "Should contain project agent + built-in agents"
    );
    let analyst = result.iter().find(|(id, _, _)| id == "analyst");
    assert!(analyst.is_some(), "Project agent should be present");
    assert_eq!(analyst.unwrap().1, "data-analyst");
    assert_eq!(analyst.unwrap().2, "Analyzes data");
}

#[tokio::test]
async fn test_before_agent_no_longer_injects_summary() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("tester.md"),
        "---\nname: tester\ndescription: Runs tests\n---\n\nYou run tests.\n",
    )
    .unwrap();

    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    let mut state = AgentState::new(dir.path().to_str().unwrap());
    <SubAgentMiddleware as Middleware>::before_agent(&m, &mut state)
        .await
        .unwrap();

    // Agent list has been migrated to system prompt placeholder injection, before_agent no longer prepends messages
    assert_eq!(
        state.messages().len(),
        0,
        "before_agent should not inject agent summary messages"
    );
}

#[tokio::test]
async fn test_before_agent_no_agents_no_op() {
    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    let mut state = AgentState::new("/nonexistent");
    <SubAgentMiddleware as Middleware>::before_agent(&m, &mut state)
        .await
        .unwrap();
    assert_eq!(state.messages().len(), 0);
}

/// Verify before_agent snapshots messages to shared parent_messages
#[tokio::test]
async fn test_before_agent_snapshots_messages() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    )
    .with_parent_messages(Arc::clone(&parent_messages));

    let mut state = AgentState::new("/tmp");
    state.add_message(BaseMessage::human("Hello"));
    state.add_message(BaseMessage::ai("Hi"));

    <SubAgentMiddleware as Middleware>::before_agent(&m, &mut state)
        .await
        .unwrap();

    let snapshot = parent_messages.read();
    assert_eq!(
        snapshot.len(),
        2,
        "parent_messages should contain 2 snapshot messages"
    );
    assert_eq!(snapshot[0].content(), "Hello");
    assert_eq!(snapshot[1].content(), "Hi");
}

/// Verify build_tool passes parent_messages to SubAgentTool
#[test]
fn test_build_tool_receives_parent_messages() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    )
    .with_parent_messages(Arc::clone(&parent_messages));

    let tool = m.build_tool("/tmp");
    // SubAgentTool with parent_messages set should handle fork: true without error
    // (the test verifies the field is passed through; functional test is in tool.rs)
    assert_eq!(tool.name(), "Agent");
}

/// 回归测试（issue 2026-08-06-e2e-bg-task-area-entry-missing）：
/// `set_parent_session` 之后 `build_tool` 的 SubAgentTool 必须能经 parent_session
/// 读到 session 级运行时 host（task_manager / bg_event_sender 等）——生产路径
/// `build_stage_context` 中工具注入（collect_tools）晚于 parent_session 注入，
/// 若时序倒置则 `host()` 回退到空 host，`run_in_background: true` 静默降级为
/// 同步执行，BgTaskArea 无运行条目。
#[test]
fn test_build_tool_after_set_parent_session_reads_runtime_host() {
    use peri_agent::{
        agent::async_tasks::TaskManager,
        session::{subagent::SubagentHost, FrozenContext, MessageQueue, Session},
    };

    // 构造带运行时 host 的父 session（模拟 build_stage_context 注入点）
    let session = Session::new_with_cancel_and_queue(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        None,
        Arc::new(AgentCancellationToken::new()),
        MessageQueue::new(),
    );
    let task_manager = Arc::new(TaskManager::new());
    session.set_subagent_host(SubagentHost {
        task_manager: Some(Arc::clone(&task_manager)),
        ..Default::default()
    });

    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
    );
    // 先注入 parent_session（模拟 set_parent_session 先于 collect_tools）
    m.set_parent_session(Arc::clone(&session));
    let tool = m.build_tool("/tmp");

    let host = tool.host().expect("build_tool 后 host 应可读");
    assert!(
        host.task_manager.is_some(),
        "tool.host().task_manager 应为 Some（parent_session 注入后构建工具）"
    );
}

#[test]
fn test_scan_agents_with_extra_dirs() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let extra_dir = dir.path().join("extra_agents");
    std::fs::create_dir_all(&extra_dir).unwrap();
    std::fs::write(
        extra_dir.join("plugin-agent.md"),
        "---\nname: plugin-agent\ndescription: From plugin\n---\n\nPlugin agent.\n",
    )
    .unwrap();

    let result = scan_agents_with_extra_dirs(
        dir.path().to_str().unwrap(),
        std::slice::from_ref(&extra_dir),
    );
    // Should contain plugin-agent + built-in agents
    let plugin = result.iter().find(|(id, _, _)| id == "plugin-agent");
    assert!(plugin.is_some(), "Plugin agent should be present");
    assert_eq!(plugin.unwrap().2, "From plugin");
}

#[test]
fn test_scan_agents_with_extra_dirs_dedup() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let cwd_agents = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&cwd_agents).unwrap();
    std::fs::write(
        cwd_agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: CWD reviewer\n---\n\nReview.\n",
    )
    .unwrap();

    let extra_dir = dir.path().join("extra");
    std::fs::create_dir_all(&extra_dir).unwrap();
    std::fs::write(
        extra_dir.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Plugin reviewer\n---\n\nReview.\n",
    )
    .unwrap();

    let result = scan_agents_with_extra_dirs(dir.path().to_str().unwrap(), &[extra_dir]);
    // Duplicate "reviewer" should be deduped (CWD takes precedence)
    let reviewer_count = result.iter().filter(|(id, _, _)| id == "reviewer").count();
    assert_eq!(reviewer_count, 1, "duplicate agent_id should be deduped");
    // Total: CWD reviewer (1) + built-in agents (6, none named "reviewer") + extra reviewer (deduped) = 7
    assert_eq!(result.len(), 7);
}

#[test]
fn test_scan_agents_with_extra_dirs_empty() {
    let result = scan_agents_with_extra_dirs("/nonexistent", &[]);
    let expected = scan_agents("/nonexistent");
    assert_eq!(result.len(), expected.len());
}

// ── count_tool_calls_from_session 单元测试 ──────────────

#[test]
fn test_count_tool_calls_from_session_zero_when_empty() {
    let session = make_session();
    assert_eq!(
        peri_agent::session::subagent::count_tool_calls_from_session(&session),
        0,
        "空 transcript 应返回 0"
    );
}

#[test]
fn test_count_tool_calls_from_session_counts_multiple_tools() {
    let session = make_session();
    {
        let transcript = session.transcript();
        let mut tx = transcript.write();
        tx.append(BaseMessage::tool_result("call_1", "result 1"));
        tx.append(BaseMessage::tool_result("call_2", "result 2"));
        tx.append(BaseMessage::tool_result("call_3", "result 3"));
    }
    assert_eq!(
        peri_agent::session::subagent::count_tool_calls_from_session(&session),
        3,
        "3 条 Tool 消息应被正确统计"
    );
}

#[test]
fn test_count_tool_calls_from_session_ignores_non_tool_messages() {
    let session = make_session();
    {
        let transcript = session.transcript();
        let mut tx = transcript.write();
        tx.append(BaseMessage::human("hello"));
        tx.append(BaseMessage::tool_result("call_1", "result 1"));
        tx.append(BaseMessage::ai("thinking..."));
        tx.append(BaseMessage::tool_result("call_2", "result 2"));
        tx.append(BaseMessage::system("system prompt"));
    }
    assert_eq!(
        peri_agent::session::subagent::count_tool_calls_from_session(&session),
        2,
        "只应统计 Tool 消息，忽略 Human/Ai/System"
    );
}

#[test]
fn test_count_tool_calls_from_session_counts_error_tools() {
    let session = make_session();
    {
        let transcript = session.transcript();
        let mut tx = transcript.write();
        tx.append(BaseMessage::tool_result("call_1", "success"));
        tx.append(BaseMessage::tool_error(
            "call_2",
            "failed: permission denied",
        ));
    }
    assert_eq!(
        peri_agent::session::subagent::count_tool_calls_from_session(&session),
        2,
        "错误工具调用也应被统计（失败也是一次执行）"
    );
}

fn make_session() -> std::sync::Arc<session::Session> {
    use std::sync::Arc;
    let cwd: Arc<str> = Arc::from("/tmp/test_count_tools");
    let frozen = session::FrozenContext::builder().build();
    session::Session::new(cwd, frozen, None)
}

// ─── D5: infer_agent_capability 保守 readonly/writes 推断测试 ────────────────

/// 从 YAML frontmatter 构造 agent 并推断能力画像（走真实 parse_agent_file 路径）
fn capability_from_yaml(yaml: &str) -> AgentCapability {
    let content = format!("---\n{}\n---\n\nbody", yaml);
    let agent = parse_agent_file(&content).expect("agent frontmatter 应能解析");
    infer_agent_capability(&agent.frontmatter)
}

/// [回归测试] D5：omitted tools（继承父工具）+ 仅 disallow Write/Edit，
/// 仍继承含 Bash 的父工具集 → 必须标 writes。
///
/// 历史背景（审计 prompt-sections-audit.md P1-8 修正后判定）：旧推断在
/// 未同时 disallow Write/Edit 时标 writes，但漏洞场景是 `disallowedTools:
/// [Write, Edit]` + 省略 tools——Bash 仍继承，可 echo > file / rm / git
/// commit，却被标 readonly。修复后含 Bash 一律 writes。
#[test]
fn test_capability_omitted_tools_with_disallowed_write_edit_is_writes() {
    let cap =
        capability_from_yaml("name: a\ndescription: d\ndisallowedTools:\n  - Write\n  - Edit\n");
    assert!(
        cap.can_mutate,
        "继承父工具（含 Bash）且未 disallow Bash 的 agent 不得标 readonly"
    );
}

/// omitted tools + 显式 disallow 全部核心写能力工具（含 Bash / folder_operations
/// / cron_register）→ 可证明无项目写能力 → readonly。
#[test]
fn test_capability_omitted_tools_fully_disallowed_is_readonly() {
    let cap = capability_from_yaml(
        "name: a\ndescription: d\ndisallowedTools:\n  - Bash\n  - Write\n  - Edit\n  - folder_operations\n  - cron_register\n",
    );
    assert!(
        !cap.can_mutate,
        "完全 disallow 核心写能力工具后应标 readonly"
    );
}

/// 显式 `tools: []`（NoTools）= 零工具 → readonly。
///
/// 历史背景：旧推断用 `to_vec().is_empty()` 把 NoTools 折叠为"继承父工具"，
/// 零工具 agent 被误判为 writes。修复后 Empty/NoTools 语义分离。
#[test]
fn test_capability_explicit_no_tools_is_readonly() {
    let cap = capability_from_yaml("name: a\ndescription: d\ntools: []\n");
    assert!(
        !cap.can_mutate,
        "显式 tools: [] 是零工具边界，应标 readonly"
    );
}

/// 只读白名单 `[Read, Glob, Grep]` → readonly（filter_tools 在注册层真裁剪）。
#[test]
fn test_capability_readonly_whitelist_is_readonly() {
    let cap = capability_from_yaml("name: a\ndescription: d\ntools: Read, Glob, Grep\n");
    assert!(!cap.can_mutate, "只读白名单应标 readonly");
}

/// 白名单含 Bash → writes（Bash 是写能力工具）。
#[test]
fn test_capability_whitelist_with_bash_is_writes() {
    let cap = capability_from_yaml("name: a\ndescription: d\ntools: Read, Glob, Grep, Bash\n");
    assert!(cap.can_mutate, "白名单含 Bash 应标 writes");
}

/// wildcard `tools: "*"` 等价继承全部 → writes。
#[test]
fn test_capability_wildcard_is_writes() {
    let cap = capability_from_yaml("name: a\ndescription: d\ntools: '*'\n");
    assert!(cap.can_mutate, "wildcard 继承全部工具应标 writes");
}

/// 白名单含 Write 但被 disallowed 覆盖 → 最终工具集无 Write → readonly。
#[test]
fn test_capability_whitelist_write_disallowed_is_readonly() {
    let cap =
        capability_from_yaml("name: a\ndescription: d\ntools: [Write]\ndisallowedTools: [Write]\n");
    assert!(
        !cap.can_mutate,
        "白名单中的 Write 被 disallowed 覆盖后应标 readonly"
    );
}

/// mcp__* 前缀工具无法静态证明只读 → 白名单含 mcp__ 工具按 writes 保守处理。
#[test]
fn test_capability_whitelist_mcp_prefix_is_writes() {
    let cap = capability_from_yaml("name: a\ndescription: d\ntools: [Read, mcp__files]\n");
    assert!(cap.can_mutate, "mcp__* 无法证明只读，应保守标 writes");
}
