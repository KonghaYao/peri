use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::state::AgentState,
    middleware::r#trait::Middleware,
    tools::{
        BaseTool, EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher,
        EffectiveToolError, EffectiveToolErrorCode, ToolContext,
    },
};
use peri_js_runtime::{JsExecutionFailure, JsRuntimeError};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    format_run_code_error, stable_tool_catalog, InvocationState, RunCodeTool,
    MAX_PRE_CANCELLED_INVOCATIONS, RUN_CODE_TOOL_NAME,
};

struct FakeDispatcher;

#[async_trait]
impl EffectiveToolDispatcher for FakeDispatcher {
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        _cancel: CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        if call.tool_name == "Read" {
            Ok(call.input.to_string())
        } else {
            let code = match call.tool_name.as_str() {
                "Write" => EffectiveToolErrorCode::UnknownTool,
                "Reject" => EffectiveToolErrorCode::UserRejected,
                "Cancel" => EffectiveToolErrorCode::Cancelled,
                "Timeout" => EffectiveToolErrorCode::Timeout,
                _ => EffectiveToolErrorCode::ToolFailed,
            };
            Err(EffectiveToolError::new(code, "tool error"))
        }
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        vec![
            EffectiveToolDefinition {
                name: "Write".into(),
                description: "write".into(),
                parameters: json!({}),
            },
            EffectiveToolDefinition {
                name: "Read".into(),
                description: "read".into(),
                parameters: json!({}),
            },
        ]
    }
}

#[test]
fn test_pre_cancel_registration_is_atomic_and_consumes_tombstone() {
    let mut state = InvocationState::default();
    state.cancel("target".to_string());
    let cancel = CancellationToken::new();

    state.register("target", cancel.clone());

    assert!(cancel.is_cancelled());
    assert!(state.pre_cancelled.is_empty());
    assert!(state.active.contains_key("target"));
}

#[test]
fn test_late_cancel_after_completion_leaves_no_pre_cancel_state() {
    let mut state = InvocationState::default();
    let cancel = CancellationToken::new();
    state.register("target", cancel);
    state.complete("target");

    state.cancel("target".to_string());

    assert!(state.pre_cancelled.is_empty());
    assert_eq!(state.completed.len(), 1);
}

#[test]
fn test_unknown_cancels_are_bounded() {
    let mut state = InvocationState::default();
    for index in 0..MAX_PRE_CANCELLED_INVOCATIONS + 1 {
        state.cancel(format!("invocation-{index}"));
    }

    assert_eq!(state.pre_cancelled.len(), MAX_PRE_CANCELLED_INVOCATIONS);
    assert!(!state.pre_cancelled.contains(&"invocation-0".to_string()));
}

#[test]
fn test_run_code_is_additional_direct_tool() {
    let tool = RunCodeTool;
    assert_eq!(tool.name(), RUN_CODE_TOOL_NAME);
    assert!(tool.is_direct());
}

#[test]
fn test_catalog_is_stably_sorted_from_dispatcher_view() {
    let names: Vec<String> = stable_tool_catalog(&FakeDispatcher)
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["Read", "Write"]);
}

#[tokio::test]
async fn test_run_code_routes_concurrent_calls_through_effective_dispatcher() {
    let tool = RunCodeTool;
    let result = tool
        .invoke(
            json!({
                "source": "return await Promise.all([tools.Read({ file: 'a' }), tools.Read({ file: 'b' })]);"
            }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(FakeDispatcher),
                "outer-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        result["value"],
        json!(["{\"file\":\"a\"}", "{\"file\":\"b\"}"])
    );
}

#[tokio::test]
async fn test_run_code_preserves_effective_tool_error_code() {
    let result = RunCodeTool
        .invoke(
            json!({
                "source": "try { await tools.Write({}); } catch (error) { return { name: error.name, code: error.code }; }"
            }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(FakeDispatcher),
                "outer-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        result["value"],
        json!({ "name": "ToolCallError", "code": "UNKNOWN_TOOL" })
    );
}

#[tokio::test]
async fn test_run_code_preserves_all_canonical_error_codes() {
    let result = RunCodeTool
        .invoke(
            json!({
                "source": "const codes = []; for (const name of ['Write', 'Reject', 'Cancel', 'Timeout']) { try { await tools[name]({}); } catch (error) { codes.push(error.code); } } return codes;"
            }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(FakeDispatcher),
                "outer-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        result["value"],
        json!(["UNKNOWN_TOOL", "USER_REJECTED", "CANCELLED", "TIMEOUT"])
    );
}

#[tokio::test]
async fn test_run_code_rejects_without_dispatch_context() {
    let result = RunCodeTool
        .invoke(json!({ "source": "return 1;" }), ToolContext::new(&[], "."))
        .await;
    assert!(result.is_err());
}

#[test]
fn test_run_code_description_documents_esm_and_json_contract() {
    let description = RunCodeTool.description();
    for expected in [
        "async function body",
        "ESM-only",
        "require",
        "static import",
        "await import('node:crypto')",
        "JSON-compatible",
        "undefined",
        "NaN",
        "Infinity",
        "Map",
        "Set",
    ] {
        assert!(description.contains(expected), "missing {expected}");
    }
}

#[test]
fn test_run_code_source_schema_documents_esm_and_json_contract() {
    let parameters = RunCodeTool.parameters();
    let description = parameters["properties"]["source"]["description"]
        .as_str()
        .unwrap();
    for expected in [
        "async function body",
        "ESM-only",
        "require",
        "static import",
        "await import('node:crypto')",
        "JSON-compatible",
        "undefined",
        "NaN",
        "Infinity",
        "Map",
        "Set",
    ] {
        assert!(description.contains(expected), "missing {expected}");
    }
}

#[test]
fn test_run_code_error_formatter_uses_only_stable_projection() {
    let cases = [
        (
            JsRuntimeError::ExecutionFailed(JsExecutionFailure::ToolFailed),
            "TOOL_FAILED: JavaScript execution failed",
        ),
        (
            JsRuntimeError::ExecutionFailed(JsExecutionFailure::ResourceLimit),
            "RESOURCE_LIMIT: JavaScript resource limit exceeded",
        ),
        (
            JsRuntimeError::Cancelled,
            "CANCELLED: JavaScript execution cancelled",
        ),
        (
            JsRuntimeError::Timeout {
                limit: std::time::Duration::from_secs(99),
            },
            "TIMEOUT: JavaScript execution timed out",
        ),
        (
            JsRuntimeError::Rpc("protocol-canary".into()),
            "PROTOCOL_ERROR: JavaScript RPC protocol error",
        ),
        (
            JsRuntimeError::SpawnFailed("runtime-canary".into()),
            "RUNTIME_FAILED: JavaScript runtime failed",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(format_run_code_error(error).to_string(), expected);
    }
}

#[tokio::test]
async fn test_run_code_exception_returns_safe_fixed_error() {
    let source = "throw new Error('ptc-tool-canary');";
    let error = RunCodeTool
        .invoke(
            json!({ "source": source, "input": { "input-canary": true } }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(FakeDispatcher),
                "outer-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(error, "TOOL_FAILED: JavaScript execution failed");
    for forbidden in ["ptc-tool-canary", source, "input-canary", "Error", "stack"] {
        assert!(!error.contains(forbidden));
    }
}

#[tokio::test]
async fn test_run_code_resource_limit_returns_safe_fixed_error() {
    let source = "return 'result-canary'.repeat(1024 * 1024);";
    let error = RunCodeTool
        .invoke(
            json!({ "source": source, "input": { "input-canary": true } }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(FakeDispatcher),
                "outer-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(error, "RESOURCE_LIMIT: JavaScript resource limit exceeded");
    for forbidden in ["result-canary", source, "input-canary", "repeat", "stack"] {
        assert!(!error.contains(forbidden));
    }
}

#[tokio::test]
async fn test_ptc_prompt_documents_esm_and_json_contract() {
    let middleware = super::PtcMiddleware::new();
    let mut state = AgentState::new(".");
    middleware.before_agent(&mut state).await.unwrap();
    let contribution = middleware.prompt_contribution().unwrap();
    for expected in [
        "async function body",
        "ESM-only",
        "require",
        "static import",
        "await import('node:crypto')",
        "JSON-compatible",
        "undefined",
        "NaN",
        "Infinity",
        "Map",
        "Set",
    ] {
        assert!(contribution.contains(expected), "missing {expected}");
    }
}
