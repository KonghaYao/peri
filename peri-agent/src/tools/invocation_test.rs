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

// ── A4 ⑤（IF-D6 匹配型归一）：参数别名对 effective name 生效 ──────────────────

/// 声明表里的 effective name（字面量只在 `peri_acp_types::builtin_mcp` 声明一份，
/// 由该 crate 的 `builtin_mcp_test.rs` 锁定；本文件不复写，以满足「消费点不得硬编码
/// effective name 字面量」的可 grep 事实）。
fn effective_name(instance: &str, original_name: &str) -> &'static str {
    peri_acp_types::builtin_mcp::find(instance)
        .and_then(|declared| {
            declared
                .tools
                .iter()
                .find(|tool| tool.original_name == original_name)
        })
        .map(|tool| tool.effective_name)
        .expect("builtin 声明表应声明该 (实例, 原始工具名)")
}

/// 需要参数别名的 builtin 工具：它的裸名与 effective name 都必须命中同一别名行。
///
/// 断言集合直接从声明表派生（IF-G5 第 1 条）：当前只有 `WebSearch` 需要别名，
/// 因此实际覆盖 {`WebSearch`, `mcp__web__WebSearch`}；将来声明表新增带别名的
/// 工具时，此处需一并核对（新增工具若无别名行，`normalize_params` 应为恒等）。
#[test]
fn builtin_effective_names_hit_parameter_aliases() {
    let effective = effective_name("web", "WebSearch");
    for name in ["WebSearch", effective] {
        let tool = SchemaToolStub {
            name,
            properties: vec!["query"],
        };
        let output = normalize_params(json!({"search_term": "Rust 2024"}), Some(&tool));
        assert_eq!(
            output.get("query"),
            Some(&json!("Rust 2024")),
            "{name} 必须经归一命中 search_term → query 别名"
        );
        assert!(output.get("search_term").is_none(), "{name}: 别名已被移除");
    }
}

/// 反证：归一不改变非声明表工具名的别名行为，也不新增任何 effective name 行。
#[test]
fn parameter_alias_matching_is_unchanged_for_other_names() {
    // 未知 / 外部 mcp__* 不命中任何别名行（输入原样透传）
    for name in ["mcp__some__tool", "mcp__filesystem__read_file"] {
        let tool = SchemaToolStub {
            name,
            properties: vec!["query"],
        };
        let input = json!({"search_term": "Rust"});
        assert_eq!(
            normalize_params(input.clone(), Some(&tool)),
            input,
            "{name} 不命中别名行，输入必须原样"
        );
    }
    // 大小写不匹配的 effective name 同样不命中：归一表精确匹配，且它与表内工具名
    // （即使大小写不敏感）也不相等 ⇒ 不可能命中任何别名行。
    let lowered = effective_name("web", "WebSearch").to_lowercase();
    assert!(
        original_tool_name_of_effective(&lowered).is_none(),
        "归一表是精确匹配：{lowered} 不得命中"
    );
    assert!(
        !lowered.eq_ignore_ascii_case("WebSearch"),
        "{lowered} 也不是别名行的工具名"
    );
    // 声明表内的其他两个工具（WebFetch / artifact）本来就没有别名行：归一后仍是恒等
    for (instance, original) in [("web", "WebFetch"), ("artifact", "artifact")] {
        let effective = effective_name(instance, original);
        let tool = SchemaToolStub {
            name: effective,
            properties: vec!["url"],
        };
        let input = json!({"search_term": "Rust"});
        assert_eq!(
            normalize_params(input.clone(), Some(&tool)),
            input,
            "{effective} 无别名行，输入必须原样"
        );
    }
}

/// 别名归一只作用于 **input 字段**：工具名解析面不变（模型发出的名字仍是 effective name）。
#[test]
fn parameter_alias_does_not_change_tool_resolution() {
    let effective = effective_name("web", "WebSearch");
    let target: Arc<dyn BaseTool> = Arc::new(NamedTool {
        name: effective,
        aliases: &[],
    });
    let tools = BTreeMap::from([(effective.to_string(), Arc::clone(&target))]);
    let call = ToolCall::new("call", effective, json!({"search_term": "Rust"}));
    let invocation = DirectToolInvocationResolver.resolve(&call, &tools).unwrap();
    assert!(Arc::ptr_eq(&invocation.target, &target));
    assert_eq!(
        invocation.raw_call.name, effective,
        "raw name 仍是 effective name"
    );
    assert_eq!(invocation.policy_call.name, effective);
    // 解析候选数不因别名表增加：裸名 `WebSearch` 未被注册时不可解析
    assert!(matches!(
        DirectToolInvocationResolver.resolve(&ToolCall::new("c2", "WebSearch", json!({})), &tools),
        Err(AgentError::ToolNotFound(name)) if name == "WebSearch"
    ));
}
