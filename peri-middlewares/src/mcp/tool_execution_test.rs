use super::evidence;
use super::BaseTool;
use super::RawToolResult;
use super::ToolExecutionStatus;
struct Tool {
    builtin: bool,
    name: &'static str,
    instance: &'static str,
}
#[async_trait::async_trait]
impl BaseTool for Tool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn builtin_mcp_instance(&self) -> Option<&str> {
        self.builtin.then_some(self.instance)
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        Some(self.name)
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!()
    }
}
#[test]
fn complete_response_requires_trusted_completion_contract_or_typed_evidence() {
    let raw = RawToolResult::new(serde_json::json!({"content":[],"isError":false}));
    let bash = Tool {
        builtin: true,
        name: "Bash",
        instance: "workspace",
    };
    assert!(super::complete(&bash, "done".into(), raw.clone())
        .unwrap()
        .execution
        .is_none());
    let read = Tool {
        builtin: true,
        name: "Read",
        instance: "workspace",
    };
    assert_eq!(
        super::complete(&read, "started".into(), raw.clone())
            .unwrap()
            .execution
            .unwrap()
            .status,
        ToolExecutionStatus::Completed
    );
    let mut value = raw.into_value();
    value["_meta"] = serde_json::json!({(peri_acp_types::tools::TOOL_EXECUTION_META_KEY): evidence(ToolExecutionStatus::Running, Some("task".into()))});
    let output = super::complete(&bash, "done".into(), RawToolResult::new(value.clone())).unwrap();
    assert_eq!(
        output.execution.unwrap().status,
        ToolExecutionStatus::Running
    );
    let external = Tool {
        builtin: false,
        name: "Bash",
        instance: "workspace",
    };
    assert!(
        super::complete(&external, "done".into(), RawToolResult::new(value))
            .unwrap()
            .execution
            .is_none()
    );
}

#[test]
fn v5_single_exit_resolves_capability_tool_contract() {
    let raw = RawToolResult::new(serde_json::json!({"content":[],"isError":false}));
    let read = Tool {
        builtin: true,
        name: "Read",
        instance: "peri",
    };
    assert_eq!(
        super::complete(&read, "done".into(), raw)
            .unwrap()
            .execution
            .unwrap()
            .status,
        ToolExecutionStatus::Completed,
        "v5 单出口必须按 (capability, original_name) 找到 completes_on_response"
    );
}
