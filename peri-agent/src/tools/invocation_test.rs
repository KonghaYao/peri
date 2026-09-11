use async_trait::async_trait;
use serde_json::{json, Value};

use super::*;
use crate::tools::ToolContext;

struct SchemaToolStub {
    name: &'static str,
    properties: Vec<&'static str>,
}

#[async_trait]
impl BaseTool for SchemaToolStub {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        ""
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": self
                .properties
                .iter()
                .map(|property| ((*property).to_string(), json!({"type": "string"})))
                .collect::<serde_json::Map<_, _>>()
        })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

#[test]
fn supports_tool_scoped_input_aliases() {
    for (tool_name, alias, canonical, value) in [
        ("Write", "contents", "content", json!("hello")),
        ("Glob", "glob_pattern", "pattern", json!("**/*.rs")),
        ("Glob", "target_directory", "path", json!("/tmp")),
        ("WebSearch", "search_term", "query", json!("Rust 2024")),
    ] {
        let tool = SchemaToolStub {
            name: tool_name,
            properties: vec![canonical],
        };
        let output = normalize_params(json!({(alias): value.clone()}), Some(&tool));
        assert_eq!(output.get(canonical), Some(&value));
        assert!(output.get(alias).is_none());
    }
}

#[test]
fn does_not_apply_scoped_alias_to_other_tools() {
    let tool = SchemaToolStub {
        name: "OtherSearch",
        properties: vec!["query"],
    };
    let input = json!({"search_term": "Rust"});
    assert_eq!(normalize_params(input.clone(), Some(&tool)), input);
}

#[test]
fn canonical_input_wins_without_removing_alias() {
    let tool = SchemaToolStub {
        name: "Write",
        properties: vec!["content"],
    };
    let input = json!({"contents": "old", "content": "new"});
    assert_eq!(normalize_params(input.clone(), Some(&tool)), input);
}

#[test]
fn declared_alias_is_not_rewritten() {
    let tool = SchemaToolStub {
        name: "Write",
        properties: vec!["contents", "content"],
    };
    let input = json!({"contents": "old"});
    assert_eq!(normalize_params(input.clone(), Some(&tool)), input);
}
struct NamedTool {
    name: &'static str,
    aliases: &'static [&'static str],
}

#[async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "resolver fixture"
    }
    fn parameters(&self) -> Value {
        json!({})
    }
    fn aliases(&self) -> &[&str] {
        self.aliases
    }
    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

#[test]
fn resolver_binds_registered_key_canonical_name_and_declared_alias() {
    let target: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Read",
        aliases: &["reading"],
    });
    let tools = BTreeMap::from([("registered-reader".into(), Arc::clone(&target))]);
    for name in [
        "registered-reader",
        "REGISTERED-READER",
        "Read",
        "read",
        "reading",
        "READING",
    ] {
        let call = ToolCall::new("call", name, json!({"value": 1}));
        let invocation = DirectToolInvocationResolver.resolve(&call, &tools).unwrap();
        assert!(Arc::ptr_eq(&invocation.target, &target));
        assert_eq!(invocation.raw_call.name, name);
        assert_eq!(invocation.policy_call.name, "Read");
        assert_eq!(invocation.policy_call.id, "call");
        assert_eq!(invocation.policy_call.input, call.input);
    }
}

#[test]
fn resolver_rejects_exact_key_when_another_target_declares_the_same_alias() {
    let exact: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Read",
        aliases: &[],
    });
    let shadow: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Other",
        aliases: &["Read"],
    });
    let tools = BTreeMap::from([("Read".into(), exact), ("Other".into(), shadow)]);
    let result =
        DirectToolInvocationResolver.resolve(&ToolCall::new("call", "Read", json!({})), &tools);
    assert!(
        matches!(result, Err(AgentError::ToolExecutionFailed { reason, .. }) if reason == "ambiguous tool invocation")
    );
}

#[test]
fn resolver_rejects_case_folded_keys_for_distinct_instances() {
    let first: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "First",
        aliases: &[],
    });
    let second: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Second",
        aliases: &[],
    });
    let tools = BTreeMap::from([("Read".into(), first), ("read".into(), second)]);
    let result =
        DirectToolInvocationResolver.resolve(&ToolCall::new("call", "READ", json!({})), &tools);
    assert!(
        matches!(result, Err(AgentError::ToolExecutionFailed { reason, .. }) if reason == "ambiguous tool invocation")
    );
}

#[test]
fn resolver_deduplicates_one_target_registered_under_multiple_matching_keys() {
    let target: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Read",
        aliases: &["reader"],
    });
    let tools = BTreeMap::from([
        ("Read".into(), Arc::clone(&target)),
        ("read".into(), Arc::clone(&target)),
        ("reader".into(), Arc::clone(&target)),
    ]);
    for name in ["READ", "reader"] {
        let invocation = DirectToolInvocationResolver
            .resolve(&ToolCall::new("call", name, json!({})), &tools)
            .unwrap();
        assert!(Arc::ptr_eq(&invocation.target, &target));
        assert_eq!(invocation.policy_call.name, "Read");
    }
}

#[test]
fn resolver_rejects_undeclared_names() {
    let target: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: "Read",
        aliases: &["reading"],
    });
    let tools = BTreeMap::from([("Read".into(), target)]);
    let result =
        DirectToolInvocationResolver.resolve(&ToolCall::new("call", "unknown", json!({})), &tools);
    assert!(matches!(result, Err(AgentError::ToolNotFound(name)) if name == "unknown"));
}

#[test]
fn resolver_preserves_shell_and_task_alias_consumers() {
    for (tool, requested) in [
        (
            NamedTool {
                name: "Bash",
                aliases: &["Shell"],
            },
            "SHELL",
        ),
        (
            NamedTool {
                name: "Agent",
                aliases: &["task"],
            },
            "task",
        ),
        (
            NamedTool {
                name: "MyTool",
                aliases: &["Alternative"],
            },
            "ALTERNATIVE",
        ),
    ] {
        let canonical = tool.name;
        let target: Arc<dyn BaseTool> = Arc::new(tool);
        let tools = BTreeMap::from([(canonical.to_string(), Arc::clone(&target))]);
        let invocation = DirectToolInvocationResolver
            .resolve(&ToolCall::new("consumer", requested, json!({})), &tools)
            .unwrap();
        assert!(Arc::ptr_eq(&invocation.target, &target));
        assert_eq!(invocation.policy_call.name, canonical);
        assert_eq!(invocation.raw_call.name, requested);
    }
}
