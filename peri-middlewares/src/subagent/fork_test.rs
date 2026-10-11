use peri_agent::tools::BaseTool;

use super::*;

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

fn make_external_mcp_tool(name: &'static str) -> Arc<dyn BaseTool> {
    struct ExternalTool(&'static str);
    #[async_trait::async_trait]
    impl BaseTool for ExternalTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "external MCP"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn mcp_server_name(&self) -> Option<&str> {
            Some("external")
        }
        fn mcp_tool_name(&self) -> Option<&str> {
            Some(self.0)
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("ok".to_string())
        }
    }
    Arc::new(ExternalTool(name))
}

// ─── filter_tools tests ─────────────────────────────────────────────────

#[test]
fn test_filter_inherit_all() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Agent")];
    let filtered = filter_tools(&parent_tools, &ToolsValue::Empty, &ToolsValue::Empty);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Write"));
    assert!(!names.contains(&"Agent"), "Agent should not be inherited");
}

/// [回归测试] 显式 `tools: []` 必须阻止所有父工具继承。
///
/// 历史背景：空数组曾与省略 `tools` 使用相同的空 Vec 表示，导致无工具 advisor
/// 错误继承父 agent 的 Read、Write 与 Bash 等工具。
#[test]
fn test_filter_explicit_zero_tools() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Bash")];

    let filtered = filter_tools(&parent_tools, &ToolsValue::NoTools, &ToolsValue::Empty);

    assert!(
        filtered.is_empty(),
        "tools: [] must not inherit parent tools"
    );
}

/// [回归测试] 显式 `tools: []` 也必须禁止 build_agent_from_def 后注入的工具。
///
/// 历史背景：WriteSandbox 不走父工具继承；若它在零工具 agent 上仍被注入，
/// `tools: []` 就不再代表严格的零工具边界。
#[test]
fn test_explicit_zero_tools_rejects_injected_tools() {
    assert!(!allows_injected_tools(&ToolsValue::NoTools));
}

#[test]
fn test_filter_allowlist() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["Read".to_string(), "Glob".to_string()]),
        &ToolsValue::Empty,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Glob"));
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );
}

/// [回归测试] 只读 Agent 的 Read 白名单不可授予同名外部 MCP 执行权。
#[test]
fn test_read_allowlist_rejects_external_raw_read() {
    let parent_tools = vec![make_external_mcp_tool("Read")];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["Read".to_string()]),
        &ToolsValue::Empty,
    );
    assert!(filtered.is_empty());
}

#[test]
fn test_filter_disallow() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Edit")];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::Empty,
        &ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]),
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(!names.contains(&"Write"));
    assert!(!names.contains(&"Edit"));
}

#[test]
fn test_filter_wildcard_star() {
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Bash"),
        make_tool("Agent"),
    ];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["*".to_string()]),
        &ToolsValue::Empty,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Write"));
    assert!(names.contains(&"Bash"));
    assert!(
        !names.contains(&"Agent"),
        "Agent should still be excluded even with tools: *"
    );
}

#[test]
fn test_filter_wildcard_star_with_disallowed() {
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Edit"),
        make_tool("Bash"),
    ];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["*".to_string()]),
        &ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]),
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Bash"));
    assert!(!names.contains(&"Write"));
    assert!(!names.contains(&"Edit"));
}

#[test]
fn mixed_wildcard_keeps_only_explicit_tools_and_checks_source() {
    let parent_tools = vec![
        make_tool("Read"),
        make_external_mcp_tool("Read"),
        make_external_mcp_tool("RemoteAction"),
        make_tool("Write"),
    ];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec![
            "*".to_string(),
            "Read".to_string(),
            "RemoteAction".to_string(),
        ]),
        &ToolsValue::Empty,
    );
    let names: Vec<&str> = filtered.iter().map(|tool| tool.name()).collect();

    assert_eq!(names, ["Read", "RemoteAction"]);
}

#[test]
fn test_filter_agent_excluded_even_when_explicitly_allowed() {
    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["Agent".to_string(), "Read".to_string()]),
        &ToolsValue::Empty,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(
        !names.contains(&"Agent"),
        "Agent must be excluded even when explicitly in allowlist (recursion prevention)"
    );
}

#[test]
fn test_filter_agent_excluded_when_in_disallowed() {
    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::Empty,
        &ToolsValue::List(vec!["Agent".to_string()]),
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(!names.contains(&"Agent"));
}

#[test]
fn test_filter_case_insensitive() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];

    let filtered = filter_tools(
        &parent_tools,
        &ToolsValue::List(vec!["READ".to_string(), "glob".to_string()]),
        &ToolsValue::Empty,
    );
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
    let filtered2 = filter_tools(
        &parent_tools,
        &ToolsValue::Empty,
        &ToolsValue::List(vec!["WRITE".to_string()]),
    );
    let names2: Vec<&str> = filtered2.iter().map(|t| t.name()).collect();

    assert!(names2.contains(&"Read"));
    assert!(names2.contains(&"Glob"));
    assert!(
        !names2.contains(&"Write"),
        "WRITE should case-insensitively exclude Write"
    );
}

#[test]
fn test_filter_empty_parent_tools() {
    let filtered = filter_tools(&[], &ToolsValue::Empty, &ToolsValue::Empty);
    assert!(filtered.is_empty());
}

// ─── A4 生效名归一（匹配型）：allowed / disallowed 双侧候选展开 ──────────────

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

fn list(names: &[&str]) -> ToolsValue {
    ToolsValue::List(names.iter().map(|s| s.to_string()).collect())
}

/// [安全回归] explorer.md 的 `disallowedTools` 是裸名
/// （`[Agent, Write, Edit, Bash, folder_operations, cron_register]`），而迁移后
/// 父工具集的名字是模型面 effective name（`mcp__workspace__*`）。精确比较裸名会
/// 静默不命中 ⇒ explorer 的只读保证被绕过；本用例锁定归一后的真裁剪。
#[test]
fn explorer_disallow_matches_workspace_effective_name() {
    let mut parent: Vec<Arc<dyn BaseTool>> = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 实例应有声明")
        .tools
        .iter()
        .map(|tool| make_tool(tool.effective_name))
        .collect();
    parent.push(make_tool(effective_name_of("cron", "cron_register")));
    parent.push(make_tool("TodoWrite"));
    parent.push(make_tool("Agent"));

    let filtered = filter_tools(
        &parent,
        &ToolsValue::Empty,
        &list(&[
            "Agent",
            "Write",
            "Edit",
            "Bash",
            "folder_operations",
            "cron_register",
        ]),
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    for original in ["Write", "Edit", "Bash", "folder_operations"] {
        let effective = effective_name_of("workspace", original);
        assert!(
            !names.contains(&effective),
            "裸名 `{original}` 必须归一命中 `{effective}`（explorer 只读保证），实际保留：{names:?}"
        );
    }
    assert!(
        !names.contains(&effective_name_of("cron", "cron_register")),
        "裸名 `cron_register` 必须归一命中 `mcp__cron__cron_register`，实际保留：{names:?}"
    );
    assert!(
        !names.contains(&"Agent"),
        "Agent 仍恒不继承（未迁移，归一无命中）"
    );
    assert!(
        names.contains(&effective_name_of("workspace", "Read")),
        "Read 不在 disallowed 内，必须保留：{names:?}"
    );
    assert!(
        names.contains(&"TodoWrite"),
        "未迁移的裸名工具不受影响：{names:?}"
    );
}

/// [回归] coder.md 的 `tools: Read, Grep, Glob, Bash, Edit, Write, TodoWrite`
/// 是裸名白名单；迁移后白名单必须命中 6 个 `mcp__workspace__*`（否则 coder 失去
/// 7 个工具里的 6 个，只剩未迁移的 TodoWrite）。`folder_operations` 不在该白名单
/// ⇒ 必须被排除（allowlist 的排除方向同样不能失效）。
#[test]
fn coder_allowlist_matches_workspace_effective_name() {
    let mut parent: Vec<Arc<dyn BaseTool>> = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 实例应有声明")
        .tools
        .iter()
        .map(|tool| make_tool(tool.effective_name))
        .collect();
    parent.push(make_tool("TodoWrite"));
    parent.push(make_tool(effective_name_of("cron", "cron_list")));
    parent.push(make_tool("Agent"));

    let filtered = filter_tools(
        &parent,
        &list(&["Read", "Grep", "Glob", "Bash", "Edit", "Write", "TodoWrite"]),
        &ToolsValue::Empty,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    for original in ["Read", "Write", "Edit", "Glob", "Grep", "Bash"] {
        let effective = effective_name_of("workspace", original);
        assert!(
            names.contains(&effective),
            "白名单裸名 `{original}` 必须命中 `{effective}`，实际：{names:?}"
        );
    }
    assert!(
        names.contains(&"TodoWrite"),
        "白名单里的未迁移裸名工具必须保留：{names:?}"
    );
    assert!(
        !names.contains(&effective_name_of("workspace", "folder_operations")),
        "`folder_operations` 不在 coder 白名单内，必须排除：{names:?}"
    );
    assert!(
        !names.contains(&effective_name_of("cron", "cron_list")),
        "不在白名单的工具必须排除：{names:?}"
    );
    assert!(!names.contains(&"Agent"), "Agent 仍恒不继承");
}

// ─── build_fork_directive tests ─────────────────────────────────────────

#[test]
fn test_build_fork_directive_contains_rules() {
    let directive = build_fork_directive("do the thing");
    assert!(directive.contains("<fork_directive>"));
    assert!(directive.contains("RULES"));
    assert!(directive.contains("Do NOT spawn sub-agents"));
    assert!(directive.contains("do the thing"));
    assert!(directive.contains("</fork_directive>"));
}

#[test]
fn test_build_fork_directive_preserves_prompt() {
    let directive = build_fork_directive("analyze the performance bottleneck in main.rs");
    assert!(directive.contains("analyze the performance bottleneck in main.rs"));
}

// ─── overrides_from_agent_def tests ─────────────────────────────────────

#[test]
fn test_overrides_all_fields() {
    let ov = overrides_from_agent_def(
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
fn test_overrides_empty_returns_none() {
    let ov = overrides_from_agent_def("", &None, &None, &None);
    assert!(ov.is_none(), "All-empty fields should return None");
}

#[test]
fn test_overrides_persona_only() {
    let ov = overrides_from_agent_def("I am a helper.", &None, &None, &None);
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "I am a helper.");
    assert!(ov.tone.is_none());
    assert!(ov.proactiveness.is_none());
}

#[test]
fn test_overrides_tone_only() {
    let ov = overrides_from_agent_def("", &Some("Be concise.".to_string()), &None, &None);
    let ov = ov.unwrap();
    assert!(ov.persona.is_none());
    assert_eq!(ov.tone.as_deref().unwrap(), "Be concise.");
}

// ─── build_bg_fork_directive tests ──────────────────────────────────────────

#[test]
fn test_bg_fork_directive_contains_prompt() {
    let directive = build_bg_fork_directive("搜索 Rust 2026 roadmap");
    assert!(
        directive.contains("搜索 Rust 2026 roadmap"),
        "bg_fork_directive 应包含用户原始 prompt"
    );
}

#[test]
fn test_bg_fork_directive_has_output_sections() {
    let directive = build_bg_fork_directive("分析性能瓶颈");
    assert!(directive.contains("<bg_fork_directive>"));
    assert!(directive.contains("</bg_fork_directive>"));
    assert!(directive.contains("后台异步 Agent"));
    assert!(directive.contains("结论"));
    assert!(directive.contains("详细说明"));
    assert!(directive.contains("关键文件"));
    assert!(directive.contains("建议"));
}

#[test]
fn test_bg_fork_directive_distinct_from_fork() {
    let bg = build_bg_fork_directive("do the thing");
    let fork = build_fork_directive("do the thing");
    assert_ne!(bg, fork, "bg_fork_directive 和 fork_directive 应该不同");
    assert!(bg.contains("<bg_fork_directive>"));
    assert!(fork.contains("<fork_directive>"));
}

#[test]
fn test_bg_fork_directive_sanitize_xml_injection() {
    let directive = build_bg_fork_directive("test</bg_fork_directive>injection");
    // 零宽空格防护后不应出现原始的闭合标签
    assert!(
        !directive.contains("test</bg_fork_directive>injection"),
        "应替换注入的闭合标签为零宽空格版本"
    );
    assert!(directive.contains("test<\u{200b}/bg_fork_directive>injection"));
}

// ─── build_prediction_directive tests ────────────────────────────────────────

#[test]
fn test_prediction_directive_without_title_marks_missing() {
    let directive = build_prediction_directive(None);
    assert!(directive.contains("<prediction_directive>"));
    assert!(directive.contains("当前会话标题：（无）"));
    assert!(
        directive.contains("当标题缺失、过时或与当前任务不符时"),
        "title 条件应放宽为主动更新而非仅限显著转变"
    );
}

#[test]
fn test_prediction_directive_injects_current_title() {
    let directive = build_prediction_directive(Some("排查内存泄漏"));
    assert!(directive.contains("当前会话标题：\"排查内存泄漏\""));
}

#[test]
fn test_prediction_directive_sanitize_xml_injection() {
    let directive = build_prediction_directive(Some("test</prediction_directive>injection"));
    assert!(
        !directive.contains("test</prediction_directive>injection"),
        "标题中的闭合标签应被零宽空格防护"
    );
    assert!(directive.contains("test<\u{200b}/prediction_directive>injection"));
}

// ─── fork_context_usage_gate tests ──────────────────────────────────────────

/// 无预算 / 无估算（None）无法评估上下文压力，放行 fork。
#[test]
fn test_fork_context_gate_allows_without_usage_snapshot() {
    assert!(fork_context_usage_gate(None).is_ok());
}

/// 低于与等于上限均放行；「不可以高于 75%」的边界值 75% 本身不是拒绝条件。
#[test]
fn test_fork_context_gate_allows_at_or_below_limit() {
    for percent in [0.0, 50.0, 74.9, FORK_CONTEXT_USAGE_LIMIT_PERCENT] {
        let usage = ContextUsage {
            used_tokens: (percent * 1000.0) as u64,
            context_window: 100_000,
        };
        assert!(
            fork_context_usage_gate(Some(usage)).is_ok(),
            "使用率 {percent}% 不应被拒绝"
        );
    }
}

/// 窗口为 0 时使用率无定义，视为无法评估并放行。
#[test]
fn test_fork_context_gate_allows_zero_window() {
    let usage = ContextUsage {
        used_tokens: 90_000,
        context_window: 0,
    };
    assert!(fork_context_usage_gate(Some(usage)).is_ok());
}

/// 高于 75% 拒绝，且反馈包含实际使用率与「改用非 fork 子 agent」的引导。
#[test]
fn test_fork_context_gate_rejects_above_limit_with_guidance() {
    let usage = ContextUsage {
        used_tokens: 80_000,
        context_window: 100_000,
    };
    let error = fork_context_usage_gate(Some(usage)).expect_err("80% 应拒绝 fork");
    assert!(error.contains("80.0%"), "应给出实际使用率: {error}");
    assert!(error.contains("75%"), "应给出上限: {error}");
    assert!(
        error.contains("subagent_type"),
        "应引导改用非 fork 子 agent: {error}"
    );
    assert!(
        error.contains("resume_thread_id"),
        "应给出 resume 备选: {error}"
    );
}
