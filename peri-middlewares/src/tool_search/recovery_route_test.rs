//! 恢复提示里的调用文本经**真实路由**命中原对象。
//!
//! 断言对象不是文本包含，而是 `ExecuteExtraToolResolver` / `DirectToolInvocationResolver`
//! 解析出的目标工具与参数，以及目标工具实际收到的入参。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::tasks::{BgTaskKind, ExternalTaskOwner, TaskOwnerHandle};
use peri_agent::agent::compact_v2::hard_memory::{
    HardMemorySnapshot, HardMemoryTask, RecoveryTools,
};
use peri_agent::agent::react::ToolCall;
use peri_agent::session::tool_catalog::{CatalogToolEntry, SessionToolCatalogSnapshot, ToolSource};
use peri_agent::tools::{BaseTool, DirectToolInvocationResolver, ToolInvocationResolver};
use serde_json::{json, Value};

use super::{ExecuteExtraTool, ExecuteExtraToolResolver, TOOL_INVOKE_NAME};

#[derive(Default)]
struct RecordingTool {
    name: &'static str,
    original: Option<&'static str>,
    direct: bool,
    visible: bool,
    namespace: Option<&'static str>,
    calls: Arc<Mutex<Vec<Value>>>,
    calls_count: Arc<AtomicUsize>,
}

#[async_trait]
impl BaseTool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "recording fixture"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        self.direct
    }
    fn visible_to_model(&self) -> bool {
        self.visible
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        self.original
    }
    fn namespace(&self) -> Option<&str> {
        self.namespace
    }
    async fn invoke(
        &self,
        input: Value,
        _: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls_count.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().unwrap().push(input);
        Ok("ok".into())
    }
}

struct Fixture {
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
    status_calls: Arc<Mutex<Vec<Value>>>,
    status_count: Arc<AtomicUsize>,
}

fn fixture(status_name: &'static str, original: Option<&'static str>, direct: bool) -> Fixture {
    let status_calls = Arc::new(Mutex::new(Vec::new()));
    let status_count = Arc::new(AtomicUsize::new(0));
    let invoke = RecordingTool {
        name: TOOL_INVOKE_NAME,
        original: None,
        direct: true,
        visible: true,
        // 宿主调用代理的身份：无 MCP 来源 + meta 命名空间。
        namespace: Some("meta"),
        ..Default::default()
    };
    let status = RecordingTool {
        name: status_name,
        original,
        direct,
        visible: true,
        namespace: None,
        calls: Arc::clone(&status_calls),
        calls_count: Arc::clone(&status_count),
    };
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert(TOOL_INVOKE_NAME.to_string(), Arc::new(invoke));
    tools.insert(status_name.to_string(), Arc::new(status));
    Fixture {
        tools,
        status_calls,
        status_count,
    }
}

fn catalog(tools: &BTreeMap<String, Arc<dyn BaseTool>>) -> SessionToolCatalogSnapshot {
    let entries = tools
        .iter()
        .map(|(name, tool)| {
            (
                name.clone(),
                CatalogToolEntry {
                    tool: Arc::clone(tool),
                    source: ToolSource::CoreOrMiddleware,
                },
            )
        })
        .collect();
    SessionToolCatalogSnapshot {
        generation: 1,
        tools: entries,
        ..Default::default()
    }
}

fn workflow_snapshot() -> HardMemorySnapshot {
    HardMemorySnapshot {
        revision: 3,
        scope: Some("session-a".into()),
        tasks: vec![HardMemoryTask {
            kind: BgTaskKind::Workflow,
            task_id: "mcp-host-workflow".into(),
            child_thread_id: None,
            external_owner: Some(ExternalTaskOwner::workflow("run-server-1")),
            summary: "build workflow".into(),
            status: "running".into(),
            started_at: "2026-11-11T00:00:00Z".into(),
        }],
        goal: None,
        tools: RecoveryTools::none(),
    }
}

/// 从渲染正文提取唯一的 `ToolInvoke({...})` / `Name({...})` 调用对象。
fn extract_call(body: &str, callee: &str) -> Value {
    let marker = format!("{callee}(");
    let start = body.find(&marker).expect("调用必须出现在正文中") + marker.len();
    let rest = &body[start..];
    let end = rest.find(')').expect("调用必须有闭合括号");
    serde_json::from_str(&rest[..end]).expect("调用参数必须是合法 JSON")
}

#[tokio::test]
async fn rendered_v5_call_resolves_to_the_owner_run_through_tool_invoke() {
    let fixture = fixture("mcp__peri__WorkflowStatus", Some("WorkflowStatus"), false);
    let tools = RecoveryTools::from_catalog(&catalog(&fixture.tools));
    assert!(tools.workflow_status.is_some(), "前提：deferred 工具可达");
    let body = workflow_snapshot()
        .render_with(&tools)
        .expect("非空快照必须渲染");

    let arguments = extract_call(&body, TOOL_INVOKE_NAME);
    assert_eq!(arguments["name"], json!("mcp__peri__WorkflowStatus"));
    assert_eq!(
        arguments["arguments"],
        json!({"runId": "run-server-1"}),
        "参数必须是服务端 owner id"
    );

    let call = ToolCall::new("call-1", TOOL_INVOKE_NAME, arguments);
    let invocation = ExecuteExtraToolResolver::default()
        .resolve_model(&call, &fixture.tools)
        .expect("V5 路由必须解析到目标工具");
    assert_eq!(invocation.target.name(), "mcp__peri__WorkflowStatus");
    assert_eq!(invocation.policy_call.name, "mcp__peri__WorkflowStatus");
    assert_eq!(
        invocation.policy_call.input,
        json!({"runId": "run-server-1"})
    );

    // 实际执行目标工具：每次派工恰好一次调用，且拿到原 run id。
    let executor = ExecuteExtraTool::new(Arc::new(parking_lot::RwLock::new(fixture.tools.clone())));
    let outer =
        json!({"name": "mcp__peri__WorkflowStatus", "arguments": {"runId": "run-server-1"}});
    executor
        .invoke(outer, peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .expect("代理执行必须成功");
    assert_eq!(fixture.status_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.status_calls.lock().unwrap().as_slice(),
        &[json!({"runId": "run-server-1"})]
    );
}

#[tokio::test]
async fn rendered_legacy_call_resolves_directly_in_that_session() {
    let fixture = fixture(
        "mcp__workflow__WorkflowStatus",
        Some("WorkflowStatus"),
        true,
    );
    let tools = RecoveryTools::from_catalog(&catalog(&fixture.tools));
    let body = workflow_snapshot()
        .render_with(&tools)
        .expect("非空快照必须渲染");
    assert!(
        !body.contains(&format!(
            "{TOOL_INVOKE_NAME}({{\"name\":\"mcp__workflow__WorkflowStatus\""
        )),
        "legacy 会话不应被改写成代理调用：{body}"
    );

    let arguments = extract_call(&body, "mcp__workflow__WorkflowStatus");
    assert_eq!(arguments, json!({"runId": "run-server-1"}));

    let call = ToolCall::new("call-1", "mcp__workflow__WorkflowStatus", arguments.clone());
    let invocation = DirectToolInvocationResolver
        .resolve_model(&call, &fixture.tools)
        .expect("legacy 直达路由必须解析到目标工具");
    assert_eq!(invocation.target.name(), "mcp__workflow__WorkflowStatus");
    assert_eq!(invocation.policy_call.input, arguments);
}

#[tokio::test]
async fn closed_capability_renders_no_call_to_route() {
    let fixture = fixture("Read", None, true);
    let body = workflow_snapshot()
        .render_with(&RecoveryTools::from_catalog(&catalog(&fixture.tools)))
        .expect("非空快照必须渲染");
    assert!(body.contains("The owner ID is unknown") || body.contains("not directly available"));
    assert!(
        !body.contains("WorkflowStatus("),
        "能力关闭时不得渲染可执行调用：{body}"
    );
    // 句柄仍登记，供下一轮定向查找。
    assert!(body.contains("run-server-1"), "已登记句柄必须保留：{body}");
    assert_eq!(fixture.status_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn monitor_call_carries_the_monitor_id_and_only_a_handled_cursor() {
    let fixture = fixture("mcp__peri__monitor_read", Some("monitor_read"), false);
    let tools = RecoveryTools::from_catalog(&catalog(&fixture.tools));
    let mut snapshot = workflow_snapshot();
    snapshot.tasks = vec![HardMemoryTask {
        kind: BgTaskKind::Mcp,
        task_id: "mcp-host-monitor".into(),
        child_thread_id: None,
        external_owner: Some(ExternalTaskOwner::new(TaskOwnerHandle::Monitor {
            task_id: "mon-17".into(),
            after_seq: Some(42),
        })),
        summary: "monitor_start".into(),
        status: "running".into(),
        started_at: "2026-11-11T00:00:00Z".into(),
    }];
    let body = snapshot.render_with(&tools).expect("非空快照必须渲染");

    let arguments = extract_call(&body, TOOL_INVOKE_NAME);
    assert_eq!(arguments["name"], json!("mcp__peri__monitor_read"));
    assert_eq!(
        arguments["arguments"],
        json!({"task_id": "mon-17", "after_seq": 42})
    );

    let call = ToolCall::new("call-1", TOOL_INVOKE_NAME, arguments);
    let invocation = ExecuteExtraToolResolver::default()
        .resolve_model(&call, &fixture.tools)
        .expect("monitor 路由必须解析");
    assert_eq!(invocation.target.name(), "mcp__peri__monitor_read");
    assert_eq!(
        invocation.policy_call.input,
        json!({"task_id": "mon-17", "after_seq": 42})
    );
}
