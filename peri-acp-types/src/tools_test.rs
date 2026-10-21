//! 工具契约类型测试（design v2 §2.5.1：ToolDescription / title 推导 / trait 默认实现）。

use super::*;

#[test]
fn tool_context_keeps_model_call_identity_independent_from_invocation() {
    let context = ToolContext::new(&[], "/tmp");
    assert!(context.tool_call_id.is_none());
    assert!(context.invocation_id.is_none());
    assert!(context.origin.is_none());
    assert!(context.session_resources.is_none());
    let context = context.with_tool_call_id("model-call");
    assert_eq!(
        context.tool_call_id.as_ref().map(ToolCallId::as_str),
        Some("model-call")
    );
    assert!(context.invocation_id.is_none());
    assert!(context.origin.is_none());
    assert!(!context.cancellation.is_cancelled());
}

#[test]
fn with_invocation_binds_execution_identity_and_origin_together() {
    let dispatcher: std::sync::Arc<dyn EffectiveToolDispatcher> =
        std::sync::Arc::new(StubDispatcher);
    let context = ToolContext::new(&[], "/tmp")
        .with_invocation(
            dispatcher,
            InvocationId::new("inv-1"),
            InvocationOrigin::Model {
                tool_call_id: ToolCallId::new("model-call"),
            },
            tokio_util::sync::CancellationToken::new(),
        )
        .with_tool_call_id("model-call");

    assert!(context.effective_tool_dispatcher.is_some());
    assert_eq!(
        context.invocation_id.as_ref().map(InvocationId::as_str),
        Some("inv-1")
    );
    assert_eq!(
        context.origin.as_ref().and_then(|o| o.model_tool_call_id()),
        Some(&ToolCallId::new("model-call"))
    );
    // 两个身份分开存放：模型 tool-call ID 不参与执行身份，执行身份也不由模型决定。
    assert_ne!(
        context.invocation_id.as_ref().unwrap().as_str(),
        context.tool_call_id.as_ref().unwrap().as_str()
    );
}

struct StubDispatcher;

#[async_trait::async_trait]
impl EffectiveToolDispatcher for StubDispatcher {
    async fn dispatch(
        &self,
        _call: EffectiveToolCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolOutput, EffectiveToolError> {
        Ok(ToolOutput::from_legacy("ok"))
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        Vec::new()
    }
}

#[test]
fn context_usage_percent_uses_tokens_over_window() {
    let context = ToolContext::new(&[], "/tmp");
    assert!(context.context_usage.is_none());
    let usage = ContextUsage {
        used_tokens: 75_000,
        context_window: 100_000,
    };
    assert_eq!(usage.percent(), Some(75.0));
    let context = context.with_context_usage(Some(usage));
    assert_eq!(context.context_usage, Some(usage));
    // 窗口为 0 时使用率无定义
    assert_eq!(
        ContextUsage {
            used_tokens: 1,
            context_window: 0
        }
        .percent(),
        None
    );
}

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
    // legacy 语义：结果与执行状态都未报告，不得推断。
    assert_eq!(output.outcome(), &ToolOutcome::Unreported);
    assert_eq!(output.is_error(), None);
    assert!(output.raw().is_none());
}

#[test]
fn legacy_tool_output_serializes_without_outcome_field() {
    // 新增 outcome 字段不改变 legacy 输出的 wire 形状。
    assert_eq!(
        serde_json::to_value(ToolOutput::from_legacy("plain")).unwrap(),
        serde_json::json!({"text": "plain"})
    );
    // 旧数据（无 outcome 键）反序列化仍为未报告。
    let decoded: ToolOutput = serde_json::from_value(serde_json::json!({"text": "plain"})).unwrap();
    assert_eq!(decoded, ToolOutput::from_legacy("plain"));
}

#[test]
fn raw_result_output_reports_real_is_error_and_keeps_raw() {
    let raw = crate::tool_outcome::RawToolResult::new(serde_json::json!({
        "content": [{"type": "text", "text": "boom"}],
        "isError": true
    }));
    let output = ToolOutput::from_raw_result("boom", raw.clone());

    assert_eq!(output.is_error(), Some(true));
    assert_eq!(output.raw(), Some(&raw));
    assert_eq!(output.text, "boom");
    let encoded = serde_json::to_value(&output).unwrap();
    assert_eq!(encoded["outcome"]["kind"], "reported");
    assert_eq!(encoded["outcome"]["is_error"], true);
    assert_eq!(encoded["outcome"]["raw"]["isError"], true);
}

#[test]
fn reported_outcome_without_raw_keeps_failure_flag() {
    let output = ToolOutput::with_outcome("failed", ToolOutcome::reported(true));
    assert_eq!(output.is_error(), Some(true));
    assert!(output.raw().is_none());
}

#[test]
fn unreported_outcome_never_reports_failure() {
    assert_eq!(ToolOutcome::default(), ToolOutcome::Unreported);
    assert!(ToolOutcome::default().is_unreported());
    assert_eq!(ToolOutcome::default().is_error(), None);
}

#[test]
fn effective_tool_error_carries_remote_mcp_raw_error() {
    let error = EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, "remote failed")
        .with_remote_error(crate::tool_outcome::RemoteToolError::from_jsonrpc(
            crate::tool_outcome::JsonRpcError {
                code: -32602,
                message: "invalid params".into(),
                data: Some(serde_json::json!({"field": "name"})),
            },
        ));

    let remote = error.remote_error().unwrap();
    assert_eq!(remote.jsonrpc.as_ref().unwrap().code, -32602);
    assert_eq!(
        remote.jsonrpc.as_ref().unwrap().data,
        Some(serde_json::json!({"field": "name"}))
    );
    // 未携带远端错误时保持 None，调用方不得从 code 反推远端语义。
    assert!(
        EffectiveToolError::new(EffectiveToolErrorCode::Cancelled, "cancelled")
            .remote_error()
            .is_none()
    );
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

#[test]
fn test_retention_uses_bound_builtin_identity_not_public_name() {
    struct DeclaredTool {
        builtin: bool,
    }
    #[async_trait::async_trait]
    impl super::BaseTool for DeclaredTool {
        fn name(&self) -> &str {
            "Write"
        }
        fn description(&self) -> &str {
            "fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn builtin_mcp_instance(&self) -> Option<&str> {
            self.builtin.then_some("workspace")
        }
        fn mcp_tool_name(&self) -> Option<&str> {
            Some("Write")
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            _: super::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }
    use super::BaseTool;
    assert_eq!(
        DeclaredTool { builtin: true }.context_retention(),
        super::ContextRetention::SideEffectReceipt
    );
    assert_eq!(
        DeclaredTool { builtin: false }.context_retention(),
        super::ContextRetention::PreviewOnly
    );
}
