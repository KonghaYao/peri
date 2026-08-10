//! 工具契约类型测试（design v2 §2.5.1：ToolDescription / title 推导 / trait 默认实现）。

use super::*;

// -- ToolDescription serde roundtrip（P0 数据结构序列化） ----------------------

#[test]
fn test_tool_description_serde_roundtrip() {
    let desc = ToolDescription {
        name: "Read".to_string(),
        description: "Read a file".to_string(),
        title: Some("Read".to_string()),
        namespace: Some("filesystem".to_string()),
    };
    let json = serde_json::to_string(&desc).unwrap();
    let back: ToolDescription = serde_json::from_str(&json).unwrap();
    assert_eq!(back, desc);
    assert!(json.contains("\"name\":\"Read\""));
    assert!(json.contains("\"namespace\":\"filesystem\""));
}

// -- derive_title_from_name ----------------------------------------------------

#[test]
fn test_derive_title_from_name_camel_case() {
    assert_eq!(
        derive_title_from_name("AskUserQuestion"),
        "Ask User Question"
    );
}

#[test]
fn test_derive_title_from_name_snake_case() {
    assert_eq!(
        derive_title_from_name("folder_operations"),
        "Folder Operations"
    );
}

#[test]
fn test_derive_title_from_name_single_word() {
    assert_eq!(derive_title_from_name("Read"), "Read");
}

#[test]
fn test_derive_title_from_name_mixed_case() {
    // 小写 → 大写边界切词；词首大写
    assert_eq!(derive_title_from_name("WebFetch"), "Web Fetch");
}

// -- BaseTool 默认实现 ---------------------------------------------------------

/// 最小工具：仅实现必填三要素，验证默认方法行为。
struct MinimalTool;

#[async_trait::async_trait]
impl BaseTool for MinimalTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }
    fn description(&self) -> &str {
        "Ask the user a question"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("ok".to_string())
    }
}

/// 显式声明 title / namespace 的工具：默认实现应原样透传。
struct DecoratedTool;

#[async_trait::async_trait]
impl BaseTool for DecoratedTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn description(&self) -> &str {
        "Read a file"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("ok".to_string())
    }
    fn title(&self) -> Option<&str> {
        Some("Read File")
    }
    fn namespace(&self) -> Option<&str> {
        Some("filesystem")
    }
}

#[test]
fn test_tool_description_default_derives_title() {
    // title 未覆盖 → 由 name 推导（design v2 §2.5.1 示例）
    let desc = MinimalTool.tool_description();
    assert_eq!(desc.name, "AskUserQuestion");
    assert_eq!(desc.description, "Ask the user a question");
    assert_eq!(desc.title.as_deref(), Some("Ask User Question"));
    assert_eq!(desc.namespace, None);
}

#[test]
fn test_tool_description_honors_explicit_title_namespace() {
    let desc = DecoratedTool.tool_description();
    assert_eq!(desc.title.as_deref(), Some("Read File"));
    assert_eq!(desc.namespace.as_deref(), Some("filesystem"));
}

#[test]
fn test_prompt_declaration_default_is_none() {
    // 默认 None：未实现声明的工具不出现在提示词声明段（design v2 §2.5.1）
    assert_eq!(MinimalTool.prompt_declaration(), None);
    assert_eq!(MinimalTool.title(), None);
    assert_eq!(MinimalTool.namespace(), None);
}
