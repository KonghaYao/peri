//! Tests for execute_tool

use super::*;

struct MockTool {
    name_str: String,
    desc_str: String,
    should_fail: bool,
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            should_fail: false,
        }
    }

    fn new_failing(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            should_fail: true,
        }
    }
}

#[async_trait]
impl BaseTool for MockTool {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn description(&self) -> &str {
        &self.desc_str
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn aliases(&self) -> &[&str] {
        if self.name_str == "CronRegister" {
            &["CronCreate"]
        } else {
            &[]
        }
    }
    async fn invoke(
        &self,
        _input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if self.should_fail {
            Err("mock tool error".into())
        } else {
            Ok(format!("{} executed", self.name_str))
        }
    }
}

fn build_test_registry() -> Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> {
    let mut map = BTreeMap::new();
    map.insert(
        "CronRegister".to_string(),
        Arc::new(MockTool::new("CronRegister", "Register a cron task")) as Arc<dyn BaseTool>,
    );
    map.insert(
        "mcp__slack__send_message".to_string(),
        Arc::new(MockTool::new(
            "mcp__slack__send_message",
            "Send Slack message",
        )) as Arc<dyn BaseTool>,
    );
    map.insert(
        "FailingTool".to_string(),
        Arc::new(MockTool::new_failing(
            "FailingTool",
            "A tool that always fails",
        )) as Arc<dyn BaseTool>,
    );
    Arc::new(RwLock::new(map))
}

#[test]
fn test_tool_name_is_execute_extra_tool() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);
    assert_eq!(tool.name(), "ExecuteExtraTool");
}

#[test]
fn test_parameters_schema() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["tool_name"].is_object());
    assert!(params["properties"]["params"].is_object());
    let required = params["required"].as_array().unwrap();
    assert!(required.contains(&json!("tool_name")));
    assert!(required.contains(&json!("params")));
}

#[tokio::test]
async fn test_invoke_executes_deferred_tool() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(json!({"tool_name": "CronRegister", "params": {"expression": "* * * * *", "prompt": "test"}}), peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert_eq!(result, "CronRegister executed");
}

#[test]
fn test_resolver_projects_wrapper_to_canonical_target() {
    use peri_agent::{agent::react::ToolCall, tools::ToolInvocationResolver};

    let registry = build_test_registry();
    let mut tools = registry.read().clone();
    tools.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::clone(&registry))),
    );

    let invocation = ExecuteExtraToolResolver::default()
        .resolve(
            &ToolCall::new(
                "call_1",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "croncreate", "params": {}}),
            ),
            &tools,
        )
        .unwrap();

    assert_eq!(invocation.raw_call.name, EXECUTE_EXTRA_TOOL_NAME);
    assert_eq!(invocation.policy_call.name, "CronRegister");
    assert_eq!(
        invocation.wrapper_name.as_deref(),
        Some(EXECUTE_EXTRA_TOOL_NAME)
    );
}
#[tokio::test]
async fn test_invoke_resolves_case_and_alias() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    for target in ["cronregister", "CronCreate"] {
        let result = tool
            .invoke(
                json!({"tool_name": target, "params": {}}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert_eq!(result, "CronRegister executed");
    }
}
#[tokio::test]
async fn test_direct_and_dispatch_wrapper_share_canonical_target_and_input() {
    struct RecordingTool {
        inputs: Arc<std::sync::Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl BaseTool for RecordingTool {
        fn name(&self) -> &str {
            "Write"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}}
            })
        }
        fn aliases(&self) -> &[&str] {
            &["Save"]
        }
        async fn invoke(
            &self,
            input: Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.inputs.lock().unwrap().push(input);
            Ok("written".to_string())
        }
    }

    let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let target: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        inputs: Arc::clone(&inputs),
    });
    let registry = Arc::new(RwLock::new(BTreeMap::from([(
        "Write".to_string(),
        Arc::clone(&target),
    )])));
    let wrapper = ExecuteExtraTool::new(Arc::clone(&registry));
    let mut snapshot = registry.read().clone();
    snapshot.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::clone(&registry))),
    );
    let invocation = ExecuteExtraToolResolver::default()
        .resolve(
            &ToolCall::new(
                "call_1",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "save", "params": {"path": "/tmp/a"}}),
            ),
            &snapshot,
        )
        .unwrap();

    assert!(Arc::ptr_eq(&invocation.target, &target));
    assert_eq!(invocation.policy_call.name, "Write");
    assert_eq!(invocation.policy_call.input, json!({"file_path": "/tmp/a"}));

    wrapper
        .invoke(
            json!({"tool_name": "SAVE", "params": {"path": "/tmp/a"}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert_eq!(
        *inputs.lock().unwrap(),
        vec![json!({"file_path": "/tmp/a"})]
    );
}

#[tokio::test]
async fn test_tool_not_found_returns_error() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"tool_name": "UnknownTool", "params": {}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Tool not found"));
}

#[tokio::test]
async fn test_missing_tool_name() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"params": {}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("malformed ExecuteExtraTool invocation"));
}

#[tokio::test]
async fn test_missing_params() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"tool_name": "CronRegister"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("malformed ExecuteExtraTool invocation"));
}

#[tokio::test]
async fn test_target_tool_error_propagates() {
    let registry = build_test_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"tool_name": "FailingTool", "params": {}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "mock tool error");
}
