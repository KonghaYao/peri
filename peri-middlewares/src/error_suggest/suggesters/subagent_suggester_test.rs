use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::subagent_suggester::SubagentSuggester;

#[test]
fn test_subagent_recognizes_unknown_type() {
    // 顺序与 BUILT_IN_AGENTS（built_in_agents.rs:29）一致：
    // coder / explorer / general-purpose / plan / verification / web-researcher
    let snap = ToolRegistrySnapshot {
        subagent_types: [
            "coder".to_string(),
            "explorer".to_string(),
            "general-purpose".to_string(),
            "plan".to_string(),
            "verification".to_string(),
            "web-researcher".to_string(),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };

    let input = serde_json::json!({ "subagent_type": "explor" });
    let err = "Error: cannot find agent definition 'explor'. Check .claude/agents/ directory or use a built-in agent (explorer, plan, general-purpose, verification)";
    let ctx = ErrorContext::new("Agent", &input, err, std::path::Path::new("."), &snap);
    let result = SubagentSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(sug.summary.contains("explorer"), "应该 fuzzy 命中 explorer");
}

#[test]
fn test_subagent_recognizes_missing_param() {
    let snap = ToolRegistrySnapshot::default();
    let input = serde_json::json!({});
    let err = "Error: please provide subagent_type parameter to specify the agent type";
    let ctx = ErrorContext::new("Agent", &input, err, std::path::Path::new("."), &snap);
    let result = SubagentSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(sug.summary.contains("subagent_type"));
}

#[test]
fn test_subagent_skips_non_agent_tools() {
    let snap = ToolRegistrySnapshot::default();
    let input = serde_json::json!({});
    let err = "Error: cannot find agent definition 'foo'";
    let ctx = ErrorContext::new("Read", &input, err, std::path::Path::new("."), &snap);
    assert!(SubagentSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_subagent_skips_non_subagent_errors() {
    let snap = ToolRegistrySnapshot::default();
    let input = serde_json::json!({ "subagent_type": "explorer" });
    let err = "Error: prompt is required";
    let ctx = ErrorContext::new("Agent", &input, err, std::path::Path::new("."), &snap);
    assert!(SubagentSuggester.suggest(&ctx).is_none());
}
