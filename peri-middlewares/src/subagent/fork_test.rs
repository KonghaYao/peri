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
