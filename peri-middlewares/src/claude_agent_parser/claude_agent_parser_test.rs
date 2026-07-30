use super::*;

#[test]
fn test_parse_valid_agent_file() {
    let content = r#"---
name: code-reviewer
description: Reviews code for quality
tools: Read, Grep, Glob
model: sonnet
---

You are a code reviewer. Focus on quality and best practices.
"#;

    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.frontmatter.name, "code-reviewer");
    assert_eq!(agent.frontmatter.description, "Reviews code for quality");
    assert_eq!(agent.tools(), vec!["Read", "Grep", "Glob"]);
    assert_eq!(agent.frontmatter.model, Some("sonnet".to_string()));
    assert_eq!(
        agent.system_prompt,
        "You are a code reviewer. Focus on quality and best practices."
    );
}

#[test]
fn test_parse_agent_with_optional_fields() {
    let content = r#"---
name: safe-researcher
description: Research with restrictions
tools: Read, Grep
disallowedTools: Write, Edit
maxTurns: 10
background: true
---

You are a researcher with restricted capabilities.
"#;

    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.frontmatter.name, "safe-researcher");
    assert_eq!(agent.disallowed_tools(), vec!["Write", "Edit"]);
    assert_eq!(agent.frontmatter.max_turns, Some(10));
    assert!(agent.frontmatter.background);
}

#[test]
fn test_parse_minimal_agent() {
    let content = r#"---
name: minimal-agent
description: A minimal agent
---

Basic system prompt.
"#;

    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.frontmatter.name, "minimal-agent");
    assert!(agent.tools().is_empty());
    assert!(agent.frontmatter.model.is_none());
}

#[test]
fn test_parse_no_frontmatter() {
    let content = "Just plain markdown without frontmatter.";
    assert!(parse_agent_file(content).is_none());
}

#[test]
fn test_parse_yaml_with_inline_dashes() {
    // YAML 值中包含 --- 不应被误判为 frontmatter 结束
    let content = r#"---
name: test-agent
description: Use --- for separators
tools: Read
---

System prompt here.
"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.frontmatter.name, "test-agent");
    assert_eq!(agent.frontmatter.description, "Use --- for separators");
}

#[test]
fn test_parse_malformed_yaml_returns_none() {
    let content = "---\ninvalid: [yaml: broken\n---\n\nprompt";
    assert!(parse_agent_file(content).is_none());
}

#[test]
fn test_max_turns_zero_falls_back() {
    let content = r#"---
name: zero-turn
description: test
maxTurns: 0
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.frontmatter.max_turns, Some(0));
    // 验证 tool.rs 中的 maxTurns:0 降级逻辑（这里只验证解析正确）
}

#[test]
fn test_format_agent_id_kebab() {
    assert_eq!(format_agent_id("code-reviewer"), "Code Reviewer");
}

#[test]
fn test_format_agent_id_snake() {
    assert_eq!(format_agent_id("security_auditor"), "Security Auditor");
}

#[test]
fn test_format_agent_id_single_word() {
    assert_eq!(format_agent_id("researcher"), "Researcher");
}

#[test]
fn test_format_agent_id_mixed_separators() {
    assert_eq!(format_agent_id("my-cool_agent"), "My Cool Agent");
}

#[test]
fn test_format_agent_id_empty() {
    assert_eq!(format_agent_id(""), "");
}

#[test]
fn test_tools_value_comma_separated() {
    let content = r#"---
name: test
description: test
tools: Read, Write, Edit
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.tools(), vec!["Read", "Write", "Edit"]);
}

#[test]
fn test_tools_value_array() {
    let content = r#"---
name: test
description: test
tools:
  - Read
  - Glob
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(agent.tools(), vec!["Read", "Glob"]);
}

/// [回归测试] 显式 `tools: []` 必须保留为零工具声明，不能与省略字段混同。
///
/// 历史背景：空数组被解析为空 Vec，工具过滤器把它当成“未配置工具”并继承父工具，
/// 导致声明为无工具的 advisor 实际获得 Read、Bash 等工具。
#[test]
fn test_tools_value_explicit_empty_array_preserves_zero_tools_intent() {
    let content = r#"---
name: advisor
description: no tools
tools: []
---
prompt"#;

    let agent = parse_agent_file(content).unwrap();

    assert_eq!(agent.frontmatter.tools, ToolsValue::NoTools);
}

#[test]
fn test_tools_value_empty_string_preserves_zero_tools_intent() {
    let content = r#"---
name: test
description: test
tools: ""
---
prompt"#;

    let agent = parse_agent_file(content).unwrap();

    assert_eq!(agent.frontmatter.tools, ToolsValue::NoTools);
}

/// [回归测试] 显式 `tools: null` 不能与缺失字段混同并继承父工具。
#[test]
fn test_tools_value_null_preserves_zero_tools_intent() {
    let content = r#"---
name: test
description: test
tools: null
---
prompt"#;

    let agent = parse_agent_file(content).unwrap();

    assert_eq!(agent.frontmatter.tools, ToolsValue::NoTools);
}

/// [回归测试] `tools` 字段存在但类型无效时不得回退为继承父工具。
///
/// 历史背景：安全敏感 agent 配置误写为对象会被静默解析成 Empty，因而获得全部父工具。
#[test]
fn test_tools_value_invalid_type_rejects_agent_file() {
    let content = r#"---
name: advisor
description: no tools
tools: {}
---
prompt"#;

    assert!(parse_agent_file(content).is_none());
}

/// [回归测试] `tools` 数组不能静默丢弃非字符串元素。
#[test]
fn test_tools_value_array_with_non_string_rejects_agent_file() {
    let content = r#"---
name: advisor
description: no tools
tools: [Read, 42]
---
prompt"#;

    assert!(parse_agent_file(content).is_none());
}

/// [回归测试] allowedWriteDirs roundtrip——plan agent 声明沙箱目录
#[test]
fn test_parse_allowed_write_dirs() {
    let content = r#"---
name: planner
description: A planner agent
allowedWriteDirs:
  - ".peri/plans/"
  - ".peri/output/"
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(
        agent.frontmatter.allowed_write_dirs,
        vec![".peri/plans/", ".peri/output/"]
    );
}

/// [回归测试] allowedWriteDirs 缺失时默认为空
#[test]
fn test_parse_allowed_write_dirs_missing_defaults_empty() {
    let content = r#"---
name: basic
description: test
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert!(agent.frontmatter.allowed_write_dirs.is_empty());
}

// ─── prompt_mode tests ───────────────────────────────────────────────────

/// 验证 prompt_mode 字段缺失时默认值为 None（下游视为 extend 行为）
#[test]
fn test_prompt_mode_extend_default() {
    let content = r#"---
name: basic
description: test
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(
        agent.frontmatter.prompt_mode, None,
        "prompt_mode 缺失时应默认为 None（extend 行为）"
    );
}

/// 验证 prompt_mode: full 时正确解析
#[test]
fn test_prompt_mode_full() {
    let content = r#"---
name: full-mode
description: A full mode agent
promptMode: full
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(
        agent.frontmatter.prompt_mode,
        Some("full".to_string()),
        "prompt_mode: full 应正确解析为 Some(\"full\")"
    );
}

/// 验证未知 prompt_mode 值不 panic，保持原始值以允许下游 fallback
#[test]
fn test_prompt_mode_unknown_fallback() {
    let content = r#"---
name: weird-agent
description: test
promptMode: some-weird-value
---
prompt"#;
    // 解析不应 panic
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(
        agent.frontmatter.prompt_mode,
        Some("some-weird-value".to_string()),
        "未知 prompt_mode 值应保留原始值，下游 with_overrides() 会 fallback 到 extend"
    );
}
