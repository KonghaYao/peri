//! Tests for execute_tool

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::tool_search::tool_index::ToolSearchIndex;

struct MockTool {
    name_str: String,
    desc_str: String,
    should_fail: bool,
    direct: bool,
    model_visible: bool,
    calls: Arc<AtomicUsize>,
    mcp_source: Option<&'static str>,
    namespace: Option<&'static str>,
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            should_fail: false,
            direct: false,
            model_visible: true,
            calls: Arc::new(AtomicUsize::new(0)),
            mcp_source: None,
            namespace: None,
        }
    }

    fn new_failing(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            should_fail: true,
            direct: false,
            model_visible: true,
            calls: Arc::new(AtomicUsize::new(0)),
            mcp_source: None,
            namespace: None,
        }
    }

    fn with_mcp_source(mut self, server: &'static str) -> Self {
        self.mcp_source = Some(server);
        self
    }

    fn with_namespace(mut self, namespace: &'static str) -> Self {
        self.namespace = Some(namespace);
        self
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
    fn is_direct(&self) -> bool {
        self.direct
    }
    fn visible_to_model(&self) -> bool {
        self.model_visible
    }
    fn mcp_server_name(&self) -> Option<&str> {
        self.mcp_source
    }
    fn namespace(&self) -> Option<&str> {
        self.namespace
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
        self.calls.fetch_add(1, Ordering::SeqCst);
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

#[test]
fn external_mcp_execute_extra_tool_is_not_unwrapped() {
    use peri_agent::{agent::react::ToolCall, tools::ToolInvocationResolver};

    let external: Arc<dyn BaseTool> = Arc::new(
        MockTool::new(EXECUTE_EXTRA_TOOL_NAME, "external system tool")
            .with_mcp_source("system-server")
            .with_namespace("meta"),
    );
    let tools = BTreeMap::from([
        (EXECUTE_EXTRA_TOOL_NAME.to_string(), Arc::clone(&external)),
        (
            "CronRegister".to_string(),
            Arc::new(MockTool::new("CronRegister", "internal target")) as Arc<dyn BaseTool>,
        ),
    ]);
    let input = json!({"tool_name": "CronRegister", "params": {"value": 1}});
    let invocation = ExecuteExtraToolResolver::default()
        .resolve(
            &ToolCall::new("external_call", EXECUTE_EXTRA_TOOL_NAME, input.clone()),
            &tools,
        )
        .unwrap();

    assert!(Arc::ptr_eq(&invocation.target, &external));
    assert_eq!(invocation.policy_call.name, EXECUTE_EXTRA_TOOL_NAME);
    assert_eq!(invocation.policy_call.input, input);
    assert_eq!(invocation.wrapper_name, None);
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

// ─── H5：app-only 目标不得经 ExecuteExtraTool 执行 ─────────────────────────

/// 构造含 app-only 目标的注册表：`calls` 计数用于证明真正的一次执行都没发生。
fn build_app_only_registry() -> (
    Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    Arc<AtomicUsize>,
) {
    let hidden_calls = Arc::new(AtomicUsize::new(0));
    let hidden: Arc<dyn BaseTool> = Arc::new(MockTool {
        name_str: "CronRegister".to_string(),
        desc_str: "Register a cron task".to_string(),
        should_fail: false,
        direct: false,
        model_visible: false,
        calls: Arc::clone(&hidden_calls),
        mcp_source: None,
        namespace: None,
    });
    let mut map = BTreeMap::new();
    map.insert("CronRegister".to_string(), hidden);
    map.insert(
        "mcp__slack__send_message".to_string(),
        Arc::new(MockTool::new(
            "mcp__slack__send_message",
            "Send Slack message",
        )) as Arc<dyn BaseTool>,
    );
    (Arc::new(RwLock::new(map)), hidden_calls)
}

/// app-only 目标按**精确名**强行调用：必须失败，且目标 invoke 计数为 0
/// （wire 零调用的同源证据：permission/HITL 与目标执行体都不会被触达）。
#[tokio::test]
async fn app_only_target_by_exact_name_is_rejected_without_invocation() {
    let (registry, hidden_calls) = build_app_only_registry();
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"tool_name": "CronRegister", "params": {"expression": "* * * * *"}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;

    assert!(result.is_err(), "app-only 目标不得被成功执行");
    assert_eq!(
        hidden_calls.load(Ordering::SeqCst),
        0,
        "app-only 目标不得被真正调用（零执行）"
    );
}

/// 猜名（大小写变体）与别名（`CronCreate` → `CronRegister`）都不得绕过
/// 可见性复检——解析成功不等于可执行。
#[tokio::test]
async fn app_only_target_by_case_variant_or_alias_is_rejected_without_invocation() {
    let (registry, hidden_calls) = build_app_only_registry();
    let tool = ExecuteExtraTool::new(registry);

    for guessed in ["cronregister", "CronCreate"] {
        let result = tool
            .invoke(
                json!({"tool_name": guessed, "params": {}}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        assert!(result.is_err(), "`{guessed}` 不得解析为可执行目标");
    }
    assert_eq!(
        hidden_calls.load(Ordering::SeqCst),
        0,
        "别名/猜名路径同样不得执行 app-only 目标"
    );
}

/// 旧索引绕过：索引里仍留有 app-only 条目（模拟未过滤/陈旧索引）时，
/// `ExecuteExtraTool` 在解析目标后仍必须复检可见性——索引不是执行授权。
#[tokio::test]
async fn stale_index_entry_does_not_bypass_execute_time_visibility_check() {
    let (registry, hidden_calls) = build_app_only_registry();
    let index = ToolSearchIndex::new();
    // 陈旧索引：把 app-only 目标直接塞进检索投影（等价于修复前的索引构建）。
    index.build(vec![Arc::clone(&registry.read()["CronRegister"])]);
    assert_eq!(
        index.search("select:CronRegister", 10).len(),
        1,
        "用例前提：陈旧索引仍能检索到 app-only 目标"
    );

    let tool = ExecuteExtraTool::new(registry);
    let result = tool
        .invoke(
            json!({"tool_name": "CronRegister", "params": {}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;

    assert!(result.is_err(), "旧索引不得成为执行授权");
    assert_eq!(hidden_calls.load(Ordering::SeqCst), 0);
}

/// resolver 路径（dispatch 生产入口）同样在解析目标后复检：canonical 绑定
/// 不得把 app-only 目标交给后续审批/执行。
#[test]
fn resolver_rejects_app_only_target_after_resolution() {
    use peri_agent::{agent::react::ToolCall, tools::ToolInvocationResolver};

    let (registry, hidden_calls) = build_app_only_registry();
    let mut tools = registry.read().clone();
    tools.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::clone(&registry))),
    );

    for guessed in ["CronRegister", "CronCreate"] {
        let outcome = ExecuteExtraToolResolver::default().resolve(
            &ToolCall::new(
                "call_app_only",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": guessed, "params": {}}),
            ),
            &tools,
        );
        let error = match outcome {
            Ok(_) => panic!("app-only 目标必须在 resolve 阶段被拒绝（tool={guessed}）"),
            Err(error) => error,
        };
        assert!(
            !error.to_string().is_empty(),
            "拒绝必须带可诊断原因（tool={guessed}）"
        );
    }
    assert_eq!(hidden_calls.load(Ordering::SeqCst), 0);
}

/// 正向用例：模型可见的 deferred 目标不受影响（同名工具可见时照常执行）。
#[tokio::test]
async fn model_visible_target_still_executes_after_visibility_check() {
    let visible_calls = Arc::new(AtomicUsize::new(0));
    let visible: Arc<dyn BaseTool> = Arc::new(MockTool {
        name_str: "CronRegister".to_string(),
        desc_str: "Register a cron task".to_string(),
        should_fail: false,
        direct: false,
        model_visible: true,
        calls: Arc::clone(&visible_calls),
        mcp_source: None,
        namespace: None,
    });
    let registry = Arc::new(RwLock::new(BTreeMap::from([(
        "CronRegister".to_string(),
        visible,
    )])));
    let tool = ExecuteExtraTool::new(registry);

    let result = tool
        .invoke(
            json!({"tool_name": "CronCreate", "params": {}}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("模型可见目标必须照常执行");
    assert_eq!(result, "CronRegister executed");
    assert_eq!(visible_calls.load(Ordering::SeqCst), 1);
}
