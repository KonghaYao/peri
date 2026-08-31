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
