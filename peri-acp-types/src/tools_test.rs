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

#[test]
fn test_tool_output_default_keeps_execution_evidence_unknown() {
    let output = ToolOutput::from_legacy("ordinary result");
    assert_eq!(output.text, "ordinary result");
    assert_eq!(output.execution, None);
}

#[test]
fn test_tool_execution_evidence_serde_roundtrip() {
    let evidence = ToolExecutionEvidence {
        status: ToolExecutionStatus::RunningAfterTimeout,
        exit_code: None,
        output_ref: Some("/tmp/peri-tool-output-1.txt".into()),
        output_truncated: true,
        task_id: Some("shell-1".into()),
    };
    let encoded = serde_json::to_string(&evidence).unwrap();
    let decoded: ToolExecutionEvidence = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, evidence);
}

#[test]
fn test_tool_output_bounded_projection_always_keeps_execution_summary() {
    let output = ToolOutput::with_execution(
        "x".repeat(200),
        ToolExecutionEvidence {
            status: ToolExecutionStatus::Failed,
            exit_code: Some(7),
            output_ref: Some("/tmp/full-output.txt".into()),
            output_truncated: true,
            task_id: None,
        },
    );
    let projected = output.bounded_text(120);
    assert!(projected.chars().count() <= 120);
    assert!(projected.contains("status: failed"));
    assert!(projected.contains("exit_code: 7"));
}

#[test]
fn test_tool_output_small_limits_do_not_cut_status_names() {
    let output = ToolOutput::with_execution(
        "body",
        ToolExecutionEvidence {
            status: ToolExecutionStatus::RunningAfterTimeout,
            exit_code: None,
            output_ref: Some("/tmp/a-very-long-output-reference.txt".into()),
            output_truncated: true,
            task_id: Some("shell-123".into()),
        },
    );

    assert_eq!(output.bounded_text(0), "");
    assert_eq!(output.bounded_text(1), "…");
    assert_eq!(output.bounded_text(7).chars().count(), 7);
    assert_eq!(
        output.bounded_text("running_after_timeout".len()),
        "running_after_timeout"
    );
    assert!(!output.bounded_text(20).contains("running"));
}

#[test]
fn test_tool_output_compact_summary_prioritizes_status_then_facts() {
    let output = ToolOutput::with_execution(
        "body",
        ToolExecutionEvidence {
            status: ToolExecutionStatus::Failed,
            exit_code: Some(7),
            output_ref: Some("/tmp/a-very-long-output-reference.txt".into()),
            output_truncated: true,
            task_id: None,
        },
    );

    assert_eq!(output.bounded_text("failed".len()), "failed");
    assert_eq!(
        output.bounded_text("failed, exit_code: 7".len()),
        "failed, exit_code: 7"
    );
    assert_eq!(output.bounded_text(8), "failed");
    assert!(output.bounded_text(120).contains("status: failed"));

    let normal_limit = ToolOutput::with_execution(
        "x".repeat(9_500),
        ToolExecutionEvidence {
            status: ToolExecutionStatus::Completed,
            exit_code: Some(0),
            output_ref: None,
            output_truncated: false,
            task_id: None,
        },
    )
    .bounded_text(10_000);
    assert!(normal_limit.chars().count() <= 10_000);
    assert!(normal_limit.contains("status: completed"));
}

#[test]
fn test_tool_output_exact_summary_is_not_appended_twice() {
    let evidence = ToolExecutionEvidence {
        status: ToolExecutionStatus::Failed,
        exit_code: Some(7),
        output_ref: Some("/tmp/full-output.txt".into()),
        output_truncated: true,
        task_id: None,
    };
    let summary = evidence.render_summary();
    let output = ToolOutput::with_execution(format!("head\n{summary}"), evidence);
    assert_eq!(output.projected_text(None), output.text);
}
