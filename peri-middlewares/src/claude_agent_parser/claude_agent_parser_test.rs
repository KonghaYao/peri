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

#[test]
fn test_tools_value_empty_string() {
    let content = r#"---
name: test
description: test
tools: ""
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert!(agent.tools().is_empty());
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
