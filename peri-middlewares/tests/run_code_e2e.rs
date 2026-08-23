use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::{
    BaseTool, EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher,
    EffectiveToolError, ToolContext,
};
use peri_middlewares::ptc::RunCodeTool;
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct NoopDispatcher;

#[async_trait]
impl EffectiveToolDispatcher for NoopDispatcher {
    async fn dispatch(
        &self,
        _call: EffectiveToolCall,
        _cancel: CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        unreachable!("test source does not call internal tools")
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        Vec::new()
    }
}

#[tokio::test]
async fn run_code_projects_real_node_failures_without_leaking_user_data() {
    let error = RunCodeTool
        .invoke(
            json!({
                "source": "throw new Error('e2e-source-canary');",
                "input": { "secret": "e2e-input-canary" }
            }),
            ToolContext::new(&[], ".").with_effective_tool_dispatcher(
                Arc::new(NoopDispatcher),
                "e2e-run-code",
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(error, "TOOL_FAILED: JavaScript execution failed");
    for forbidden in [
        "e2e-source-canary",
        "e2e-input-canary",
        "throw new Error",
        "stack",
    ] {
        assert!(!error.contains(forbidden), "leaked {forbidden}: {error}");
    }
}
