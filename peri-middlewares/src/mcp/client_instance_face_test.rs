//! 宿主面「按实例消费」的真实链路证据（`client/instance_face.rs`；端口契约
//! `McpPoolPort::{call_instance_tool, read_instance_resource, subscribe_instance_resource}`）。
//!
//! 链路：生产 `WorkflowMcpServer` handler 经生产 builtin 装配
//! （`spawn_builtin_transport_with_handler`）与生产 client 握手段
//! （`serve_client_auto`）进入 per-session `McpClientPool`；三条面都经**真实
//! wire**（rmcp duplex）与 `WorkflowRuntime` 状态往返，不使用替身 handler。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::workflow::{AgentRunParams, AgentRunResult, Usage};
use peri_mcp_workflow::{WorkflowInstanceInput, WorkflowMcpServer, WorkflowRuntime};
use peri_resources::workflow::runner::AgentExecutor;
use serde_json::{json, Value};

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BuiltinServerExit,
    BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper,
};
use crate::mcp::McpTaskOwner;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 测试用 agent 执行器：本文件不触发真实 agent 执行。
struct MockAgentExecutor;

#[async_trait]
impl AgentExecutor for MockAgentExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        AgentRunResult::Ok {
            output: "mock".into(),
            usage: Usage { output_tokens: 0 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

fn task_params(run_id: &str, agent_id: u64) -> AgentRunParams {
    AgentRunParams {
        run_id: run_id.to_string(),
        agent_id,
        prompt: "do work".into(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    }
}

/// 一条已握手的 workflow 链路（生产 handler + 生产链路装配 + 生产 client 握手）。
struct WorkflowLink {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
    runtime: Arc<WorkflowRuntime>,
    /// 夹具临时目录（cwd；仅靠 `Drop` 持有生命周期）。
    _dir: tempfile::TempDir,
}

impl WorkflowLink {
    fn peer(&self) -> rmcp::service::Peer<rmcp::service::RoleClient> {
        self.service.peer().clone()
    }
}

async fn connect_workflow() -> WorkflowLink {
    let dir = tempfile::tempdir().expect("tempdir");
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockAgentExecutor);
    let (notification_tx, _notification_rx) = tokio::sync::broadcast::channel(32);
    let runtime = Arc::new(WorkflowRuntime::new(
        executor,
        dir.path().to_str().expect("tempdir 路径可用"),
        notification_tx,
        None,
    ));
    let server = WorkflowMcpServer::new(WorkflowInstanceInput::new(Arc::clone(&runtime)));
    let transport = spawn_builtin_transport_with_handler("workflow", server);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("workflow 握手不得超时")
        .expect("workflow 握手不得失败");
    WorkflowLink {
        service,
        supervisor,
        runtime,
        _dir: dir,
    }
}

/// 把已握手的 workflow 连接装入 per-session 池（实例句柄 + 任务作用域 owner）。
fn pool_with_workflow(link: &WorkflowLink) -> (Arc<McpClientPool>, McpTaskOwner) {
    let (owner, spawner) = McpTaskOwner::new();
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);
    pool.clients.write().insert(
        "workflow".to_string(),
        Arc::new(McpClientHandle {
            name: "workflow".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(link.peer()),
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: "workflow".to_string(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    (pool, owner)
}

/// 收尾顺序（与既有线路测试同构）：释放保活引用（handle.peer）→ 关 service →
/// 收敛 server task（断言收敛，不接受 abort）→ 池与任务作用域收口。
async fn shutdown_workflow(
    mut link: WorkflowLink,
    pool: Arc<McpClientPool>,
    mut owner: McpTaskOwner,
) {
    pool.clients.write().clear();
    let _ = link.service.close_with_timeout(CLOSE_TIMEOUT).await;
    let outcome = link.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        !matches!(outcome.server, BuiltinServerExit::AbortedAfterTimeout),
        "server task 必须收敛，不得 abort: {outcome:?}"
    );
    drop(pool);
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

/// 等待订阅转发任务从任务作用域收口（`active_count` 归零）。
async fn wait_for_task_scope_idle(owner: &McpTaskOwner) {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        while owner.active_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("订阅转发任务必须在期限内收口");
}

// ─── 工具面：tools/call（含 request `_meta`） ─────────────────────────────────

#[tokio::test]
async fn call_instance_tool_round_trips_workflow_status() {
    let link = connect_workflow().await;
    let (pool, owner) = pool_with_workflow(&link);
    let port: Arc<dyn McpPoolPort> = pool.clone();

    // 只读往返：`WorkflowStatus` 无 runId 返回 run 索引（JSON 文本）。
    let text = port
        .call_instance_tool("workflow", "WorkflowStatus", json!({}), None)
        .await
        .expect("只读查询必须成功");
    let runs: Value = serde_json::from_str(&text).expect("WorkflowStatus 返回 JSON 文本");
    assert_eq!(runs, json!([]), "空索引与 runtime 一致");

    // 工具级失败保持服务端文本（不重造映射）。
    let error = port
        .call_instance_tool(
            "workflow",
            "WorkflowStatus",
            json!({ "runId": "missing" }),
            None,
        )
        .await
        .expect_err("未知 run 是工具级失败");
    assert!(
        error.contains("workflow run 'missing' not found"),
        "{error}"
    );

    shutdown_workflow(link, pool, owner).await;
}

#[tokio::test]
async fn call_instance_tool_forwards_request_meta_to_consumer_face() {
    let link = connect_workflow().await;
    let (pool, owner) = pool_with_workflow(&link);
    let port: Arc<dyn McpPoolPort> = pool.clone();
    let token = link.runtime.bind_consumer_token();
    let arguments =
        json!({ "runId": "run-1", "taskId": "task-1", "result": { "kind": "skipped" } });

    // 缺 `_meta`：服务端授权拒绝（消费者令牌只从请求 `_meta` 取）。
    let missing = port
        .call_instance_tool("workflow", "WorkflowAgentResult", arguments.clone(), None)
        .await
        .expect_err("缺令牌必须拒绝");
    assert!(missing.contains("peri/workflowConsumer"), "{missing}");

    // 带 `_meta`：授权通过并进入任务裁决——`_meta` 已抵达服务端。
    let error = port
        .call_instance_tool(
            "workflow",
            "WorkflowAgentResult",
            arguments,
            Some(json!({ "peri/workflowConsumer": token })),
        )
        .await
        .expect_err("未知任务必须拒绝");
    assert!(error.contains("unknown workflow task 'task-1'"), "{error}");

    shutdown_workflow(link, pool, owner).await;
}

// ─── 资源面：resources/read ──────────────────────────────────────────────────

#[tokio::test]
async fn read_instance_resource_round_trips_workflow_runs() {
    let link = connect_workflow().await;
    let (pool, owner) = pool_with_workflow(&link);
    let port: Arc<dyn McpPoolPort> = pool.clone();

    let text = port
        .read_instance_resource("workflow", "workflow://runs")
        .await
        .expect("资源读取必须成功");
    let runs: Value = serde_json::from_str(&text).expect("workflow://runs 返回 JSON 文本");
    assert_eq!(runs, json!([]), "空索引与 runtime 一致");

    // URI 不支持：服务端拒绝文本透出（不伪装为空资源）。
    let error = port
        .read_instance_resource("workflow", "workflow://nope")
        .await
        .expect_err("未登记 URI 必须失败");
    assert!(error.contains("unsupported workflow resource"), "{error}");

    shutdown_workflow(link, pool, owner).await;
}

// ─── 订阅面：notifications/resources/updated ─────────────────────────────────

#[tokio::test]
async fn subscribe_instance_resource_receives_tasks_updates() {
    let link = connect_workflow().await;
    let (pool, owner) = pool_with_workflow(&link);
    let port: Arc<dyn McpPoolPort> = pool.clone();

    let mut subscription = port
        .subscribe_instance_resource("workflow", "workflow://tasks")
        .await
        .expect("订阅必须建立");

    // 唤醒信号竞态容忍：服务端在 ack 之后才进入 listen 回调登记广播接收方，
    // 逐次发布直到通知送达（首条丢失时消费方本应补读，设计 §4.3）。
    let mut received = None;
    for attempt in 0..20u64 {
        link.runtime
            .tasks()
            .publish(format!("task-{attempt}"), task_params("run-1", attempt));
        match tokio::time::timeout(Duration::from_millis(100), subscription.next()).await {
            Ok(Some(uri)) => {
                received = Some(uri);
                break;
            }
            Ok(None) => panic!("订阅不得在取消前终止"),
            Err(_) => continue,
        }
    }
    assert_eq!(
        received.as_deref(),
        Some("workflow://tasks"),
        "只收订阅 URI 的通知（run 级 tasks 通知不在 filter 内）"
    );

    // 取消：句柄收口后通知流返回 None，转发任务从任务作用域退出。
    subscription.cancel();
    assert!(tokio::time::timeout(OBSERVE_TIMEOUT, subscription.next())
        .await
        .expect("取消后通知流必须收口")
        .is_none());
    wait_for_task_scope_idle(&owner).await;

    shutdown_workflow(link, pool, owner).await;
}

#[tokio::test]
async fn remove_server_stops_instance_subscriptions() {
    let link = connect_workflow().await;
    let (pool, owner) = pool_with_workflow(&link);
    let port: Arc<dyn McpPoolPort> = pool.clone();

    let mut subscription = port
        .subscribe_instance_resource("workflow", "workflow://tasks")
        .await
        .expect("订阅必须建立");
    pool.remove_server("workflow").await;

    // 实例移除：订阅流终止（宿主下一次 `next()` 观察到 None 并重建订阅）。
    assert!(tokio::time::timeout(OBSERVE_TIMEOUT, subscription.next())
        .await
        .expect("实例移除后订阅必须终止")
        .is_none());
    wait_for_task_scope_idle(&owner).await;

    shutdown_workflow(link, pool, owner).await;
}

// ─── 错误语义：未知实例 / 未连接 ─────────────────────────────────────────────

#[tokio::test]
async fn instance_face_reports_unknown_and_disconnected_instances() {
    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let port: Arc<dyn McpPoolPort> = pool.clone();

    // 未知实例：三条面一致拒绝（不伪造空结果）。
    let error = port
        .call_instance_tool("absent", "WorkflowStatus", json!({}), None)
        .await
        .expect_err("未知实例必须失败");
    assert!(error.contains("未找到"), "{error}");
    let error = port
        .read_instance_resource("absent", "workflow://runs")
        .await
        .expect_err("未知实例必须失败");
    assert!(error.contains("未找到"), "{error}");
    let error = port
        .subscribe_instance_resource("absent", "workflow://tasks")
        .await
        .expect_err("未知实例必须失败");
    assert!(error.contains("未找到"), "{error}");

    // 未连接：句柄在池中但状态非 Connected（与未知实例是两个事实）。
    pool.clients.write().insert(
        "stale".to_string(),
        Arc::new(McpClientHandle {
            name: "stale".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Disconnected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let error = port
        .call_instance_tool("stale", "WorkflowStatus", json!({}), None)
        .await
        .expect_err("未连接实例必须失败");
    assert!(error.contains("未连接"), "{error}");

    let mut owner = owner;
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}
