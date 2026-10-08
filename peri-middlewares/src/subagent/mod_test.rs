use peri_acp_types::builtin_mcp::{original_tool_name_of_effective, BUILTIN_MCP_INSTANCES};
use peri_agent::{
    agent::state::AgentState, messages::BaseMessage, middleware::r#trait::Middleware, session,
};

use super::*;
use peri_mcp_core::agent_definition::parse_agent_file;

#[derive(Clone)]
struct EchoLLM;

impl EchoLLM {
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

        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        text_events(format!("echo: {}", last))
    }
}
crate::subagent::test_support::fixture_model_impl!(EchoLLM);

#[test]
fn test_middleware_name() {
    let m = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
    );
    let tool = m.build_tool("/tmp");
    assert_eq!(tool.name(), "Agent");
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
    );
    // 先注入 parent_session（模拟 set_parent_session 先于 collect_tools）
    m.set_parent_session(Arc::clone(&session));
    let tool = m.build_tool("/tmp");

    let host = tool.host();
    assert!(
        host.task_manager.is_some(),
        "tool.host().task_manager 应为 Some（parent_session 注入后构建工具）"
    );
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
    infer_agent_capability(&agent.frontmatter).expect("夹具档位必须合法")
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

// ─── A4 生效名归一（IF-D6 判定型 ③ / IF-D15）─────────────────────────────

/// 从注册表解析 effective name（测试不得硬编码 `mcp__*` 字面量）。
fn effective_name_of(instance: &str, original: &str) -> &'static str {
    peri_acp_types::builtin_mcp::find(instance)
        .unwrap_or_else(|| panic!("声明表应含实例 `{instance}`"))
        .tools
        .iter()
        .find(|tool| tool.original_name == original)
        .unwrap_or_else(|| panic!("声明表应含 `{instance}` 的原始工具名 `{original}`"))
        .effective_name
}

/// IF-D6 ③：builtin 一等工具的 effective name 按原始名判定（判定相等）。
#[test]
fn mutation_tool_matches_original_name_policy_for_builtin_names() {
    let mut declared = 0;
    for instance in BUILTIN_MCP_INSTANCES {
        for tool in instance.tools {
            declared += 1;
            assert_eq!(
                is_mutation_tool(tool.effective_name),
                is_mutation_tool(tool.original_name),
                "`{}` 的 mutation 判定必须等于原始名 `{}`（IF-D6 ③）",
                tool.effective_name,
                tool.original_name
            );
        }
    }
    // 行数从注册表派生（不再硬编码：wave 3 的 `== 7` 是漏记 workspace 7 行之后
    // 的中间态红灯，AW3-08）。遍历必须覆盖注册表全部行；注册表自身的逐实例内容
    // 由 `peri-acp-types/src/builtin_mcp_test.rs` 的字面量测试锁定。
    let expected: usize = BUILTIN_MCP_INSTANCES.iter().map(|i| i.tools.len()).sum();
    assert_eq!(declared, expected, "遍历必须覆盖注册表全部行");
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(
            !instance.tools.is_empty(),
            "实例 `{}` 不得声明零工具（实例被清空会使派生期望值同步退化）",
            instance.name
        );
    }

    // wave 1 冻结结果（IF-G2 第 2 条）：两个 Web 工具不再因 `mcp__` 前缀被算 mutation；
    // artifact 迁移前就是裸名且不在 mutation 集合内，判定不变。
    assert!(!is_mutation_tool("mcp__web__WebSearch"));
    assert!(!is_mutation_tool("mcp__web__WebFetch"));
    assert!(!is_mutation_tool("mcp__artifact__artifact"));

    // wave 2 冻结结果（IF-P3-11）：`cron_register` 两种名字形态都必须判 mutation
    // （可定时触发任意 prompt，等价委派执行权）；`cron_list` / `cron_remove`
    // 两种形态都必须判非 mutation（不再因 `mcp__` 前缀算写能力）。
    // 期望值逐项写死，等价断言不能替代：归一与集合同时改错时等价仍成立。
    let frozen: [(&str, &str, bool); 3] = [
        ("cron", "cron_register", true),
        ("cron", "cron_list", false),
        ("cron", "cron_remove", false),
    ];
    for (instance, original, mutation) in frozen {
        let declared = peri_acp_types::builtin_mcp::find(instance).expect("实例应有声明");
        let tool = declared
            .tools
            .iter()
            .find(|tool| tool.original_name == original)
            .unwrap_or_else(|| panic!("声明表应含 `{instance}` 的原始工具名 `{original}`"));
        for name in [original, tool.effective_name] {
            assert_eq!(
                is_mutation_tool(name),
                mutation,
                "`{name}` 的 mutation 判定应为 {mutation}（wave 2 冻结）"
            );
        }
    }

    // wave 3 冻结结果：workspace 七工具（Write/Edit/folder_operations/Bash 是写能力，
    // Read/Glob/Grep 不是），两种名字形态逐项绝对判定——等价断言之外再锁方向。
    let workspace_frozen: [(&str, bool); 7] = [
        ("Read", false),
        ("Write", true),
        ("Edit", true),
        ("Glob", false),
        ("Grep", false),
        ("folder_operations", true),
        ("Bash", true),
    ];
    let workspace = peri_acp_types::builtin_mcp::find("workspace").expect("workspace 实例应有声明");
    for (original, mutation) in workspace_frozen {
        let tool = workspace
            .tools
            .iter()
            .find(|tool| tool.original_name == original)
            .unwrap_or_else(|| panic!("声明表应含 workspace 的原始工具名 `{original}`"));
        for name in [original, tool.effective_name] {
            assert_eq!(
                is_mutation_tool(name),
                mutation,
                "`{name}` 的 mutation 判定应为 {mutation}（wave 3 冻结）"
            );
        }
    }
}

/// 反证（IF-D6 冻结约束 3）：未知 / 外部 `mcp__*` 仍保守算 mutation。
#[test]
fn unknown_mcp_prefix_still_mutates() {
    assert!(is_mutation_tool("mcp__filesystem__write_file"));
    assert!(is_mutation_tool("mcp__anything"));
    // 与 effective name 仅差大小写 ⇒ 未命中归一表 ⇒ 走既有的 `mcp__` 前缀保守路径
    assert_eq!(original_tool_name_of_effective("mcp__web__fetch"), None);
    assert!(is_mutation_tool("mcp__web__fetch"));
    // 非 builtin 名字的判定逐位不变
    assert!(is_mutation_tool("Bash"));
    assert!(!is_mutation_tool("Read"));
}

/// 归一的可见后果（IF-G2 第 2 条）：白名单只含 builtin Web 工具的 agent 由
/// `writes` 改为 `readonly`（迁移前 `mcp__*` 前缀一律算写能力）。
#[test]
fn builtin_web_tool_whitelist_is_readonly() {
    let cap = capability_from_yaml(
        "name: a\ndescription: d\ntools: [mcp__web__WebFetch, mcp__web__WebSearch]\n",
    );
    assert!(
        !cap.can_mutate,
        "只含 builtin Web 工具（均非写能力）的 agent 应标 readonly"
    );
}

/// [安全回归] 用户用**模型面 effective name** 写 `disallowedTools`
/// （`mcp__workspace__Bash` …）时，必须与写裸名一样命中 `MUTATION_CORE` 判定；
/// 否则「核心写能力已被完全 disallow」的 readonly 结论失效，模块被误判为仍可写。
/// 名字从注册表派生（测试不硬编码 `mcp__*` 字面量）。
#[test]
fn fully_disallowed_with_workspace_effective_names_is_readonly() {
    let disallow = |names: &[(&str, &str)]| {
        names
            .iter()
            .map(|(instance, original)| format!("  - {}", effective_name_of(instance, original)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let cap = capability_from_yaml(&format!(
        "name: a\ndescription: d\ndisallowedTools:\n{}\n",
        disallow(&[
            ("workspace", "Bash"),
            ("workspace", "Write"),
            ("workspace", "Edit"),
            ("workspace", "folder_operations"),
            ("cron", "cron_register"),
        ])
    ));
    assert!(
        !cap.can_mutate,
        "effective name 写全五个核心写能力工具后应标 readonly（omitted tools 路径）"
    );

    // 对照：少覆盖一个（Write）⇒ 必须仍标 writes（证明上面的 readonly 来自归一
    // 命中，而不是「任何名字都被算作覆盖」）。
    let partial = capability_from_yaml(&format!(
        "name: a\ndescription: d\ndisallowedTools:\n{}\n",
        disallow(&[
            ("workspace", "Bash"),
            ("workspace", "Edit"),
            ("workspace", "folder_operations"),
            ("cron", "cron_register"),
        ])
    ));
    assert!(
        partial.can_mutate,
        "未覆盖 Write 时必须保守标 writes（对照组）"
    );
}

/// [安全回归] 白名单写 effective name、disallowed 写裸名（N2 的
/// `ToolsValue::List` 分支）也必须互相命中——否则「白名单里的写工具被 disallowed
/// 覆盖」这一判定失效，agent 被误标 writes（保守方向安全，但白名单 + 覆盖的
/// readonly 结论反转）。
#[test]
fn capability_whitelist_effective_name_disallowed_by_bare_name_is_readonly() {
    let cap = capability_from_yaml(&format!(
        "name: a\ndescription: d\ntools: [{}, Read]\ndisallowedTools: [Write]\n",
        effective_name_of("workspace", "Write")
    ));
    assert!(
        !cap.can_mutate,
        "白名单里的 `mcp__workspace__Write` 被裸名 `Write` 覆盖后应标 readonly"
    );
}

// ─── catalog 同源一致性（波 4 演进 C3，设计 §3.5.1 步骤 2）─────────────────

/// 11_subagent 段落声明：位置属性（Uncached order=4）+ 含占位符的 Builtin
/// 内容（catalog 替换留在渲染层，设计 §3.5.1 步骤 2）。
#[test]
fn subagent_section_declaration_shape() {
    let sections = SubAgentMiddleware::sections();
    assert_eq!(sections.len(), 1, "11_subagent 段应唯一");
    let section = &sections[0];
    assert_eq!(section.id, "11_subagent");
    assert_eq!(section.zone, PromptSectionZone::Uncached);
    assert_eq!(section.order, 4);
    let content = section.content.as_str();
    assert!(
        content.contains("# SubAgent Delegation"),
        "委托机制说明保留"
    );
    assert!(
        content.contains("{{available_agents}}"),
        "catalog 占位符保留（渲染层替换）"
    );
    assert!(
        content.contains("## Authorization boundary"),
        "授权边界保留（10_hitl 交叉引用依赖）"
    );
    // Selection Guide 已重构：无具体任务→agent 映射（仓库级调度建议由
    // catalog 承载），通用原则保留
    assert!(
        !content.contains("**Standard pipelines**"),
        "Standard pipelines 具体建议已删除"
    );
    assert!(
        !content.contains("Code implementation / editing / refactoring"),
        "具体任务→agent 映射已删除"
    );
    assert!(
        content.contains("Choose the most specialized ID supplied by the frozen catalog hint")
            && content.contains("current invocation-loader suggestion"),
        "冻结 hint 与动态 loader suggestion 的选择原则保留"
    );
    assert!(
        content.contains("a loader suggestion may contain only an ID")
            && content.contains("verify the loaded definition before choosing parallelism"),
        "缺少 metadata 时必须验证定义或保守执行"
    );
}

/// [M2] frontmatter `model` 未知档位必须 typed 拒绝（不静默回退父模型）。
#[test]
fn test_capability_rejects_unknown_model_tier() {
    let content = "---\nname: a\ndescription: d\nmodel: turbo\n---\n\nbody";
    let agent = parse_agent_file(content).expect("agent frontmatter 应能解析");
    assert_eq!(
        infer_agent_capability(&agent.frontmatter).unwrap_err(),
        InvalidModelTier,
        "未知档位必须 typed 拒绝"
    );
}

/// [M2] 档位大小写归一：`SoNnEt` → `sonnet`；`InHerit`/空串 → inherit。
#[test]
fn test_capability_normalizes_known_tiers_case_insensitively() {
    for (raw, expected) in [
        ("SoNnEt", "sonnet"),
        ("HAIKU", "haiku"),
        ("InHerit", "inherit"),
        ("", "inherit"),
    ] {
        let capability = capability_from_yaml(&format!("name: a\ndescription: d\nmodel: {raw}\n"));
        assert_eq!(
            capability.model_tier.catalog_label(),
            expected,
            "档位 {raw:?} 应归一为 {expected}"
        );
    }
}
