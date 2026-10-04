use super::*;
use crate::mcp::{
    config::ConfigSource, resource_tool::McpResourceTool, tool_bridge::McpToolBridge,
    McpClientHandle, OAuthStatus,
};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::tasks::{TaskManager as TaskManagerPort, TaskShutdownReport};
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CustomResult,
        ReadResourceRequestParams, ReadResourceResponse, Tool,
    },
    service::{RequestContext, RoleServer, RunningService},
    ServerHandler, ServiceExt,
};
use tokio::sync::Notify;

pub(crate) fn large_output() -> String {
    (0..2200)
        .map(|line| format!("remote output {line} λ\n"))
        .collect()
}

fn test_tool() -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": "large", "inputSchema": {"type": "object"}
    }))
    .unwrap()
}

struct Source;

struct StartedTask;

impl ServerHandler for StartedTask {
    async fn call_tool(
        &self,
        _: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(
            rmcp::model::Task::new(
                "remote-child-task",
                rmcp::model::TaskStatus::Working,
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            ),
        )))
    }
}

struct LostTaskReceipt {
    created: Arc<Notify>,
    release: Arc<Notify>,
}

impl ServerHandler for LostTaskReceipt {
    async fn call_tool(
        &self,
        _: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        // The remote task exists before the client receives its Task receipt.
        let _created = rmcp::model::Task::new(
            "remote-task",
            rmcp::model::TaskStatus::Working,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        );
        self.created.notify_one();
        self.release.notified().await;
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(
            _created,
        )))
    }
}

#[tokio::test]
async fn lost_mcp_task_receipt_keeps_session_shutdown_incomplete() {
    let created = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let wire = Wire::connect(LostTaskReceipt {
        created: Arc::clone(&created),
        release: Arc::clone(&release),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "remote", false);
    let manager: Arc<dyn TaskManagerPort> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("receipt-session", &manager);
    let bridge = McpToolBridge::new("remote", &test_tool(), pool.get_client("remote").unwrap())
        .with_output_store(&pool, Some("receipt-session"));
    let call = tokio::spawn(async move {
        bridge
            .invoke(serde_json::json!({}), ToolContext::new(&[], "."))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), created.notified())
        .await
        .expect("remote task created before receipt");
    call.abort();
    let _ = call.await;
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
    release.notify_waiters();
    pool.clients.write().clear();
    wire.close().await;
}

impl ServerHandler for Source {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let content = vec![ContentBlock::text(large_output())];
        Ok(CallToolResponse::Complete(
            if request
                .arguments
                .is_some_and(|args| args.get("error").is_some())
            {
                CallToolResult::error(content)
            } else {
                CallToolResult::success(content)
            },
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        Ok(ReadResourceResponse::Complete(
            serde_json::from_value(serde_json::json!({
                "contents": [{"uri": request.uri, "text": large_output(), "mimeType": "text/plain"}]
            }))
            .unwrap(),
        ))
    }
}

pub(crate) struct Wire {
    pub(crate) client: RunningService<RoleClient, rmcp::model::InitializeRequestParams>,
    server: tokio::task::JoinHandle<()>,
}

impl Wire {
    pub(crate) async fn connect(handler: impl ServerHandler) -> Self {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handler
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = tokio::time::timeout(
            Duration::from_secs(2),
            crate::mcp::client::mcpp_client_info_for_profile(
                &crate::mcp::apps::McpCapabilityProfile::disabled(),
            )
            .serve(client_io),
        )
        .await
        .unwrap()
        .unwrap();
        Self { client, server }
    }

    pub(crate) fn install(&self, pool: &McpClientPool, name: &str, builtin: bool) {
        let handle = Arc::new(McpClientHandle {
            name: name.into(),
            version: None,
            cache_version: None,
            peer: Some(self.client.peer().clone()),
            tools: vec![test_tool()],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::None,
            source: builtin.then(|| ConfigSource::Builtin {
                instance: "workspace".into(),
            }),
            url: None,
            skills_capable: false,
        });
        pool.advance_handle_generation(&handle);
        pool.clients.write().insert(name.into(), handle);
    }

    pub(crate) async fn close(mut self) {
        self.client
            .close_with_timeout(Duration::from_secs(1))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.server)
            .await
            .unwrap()
            .unwrap();
    }
}

/// [回归测试] 根 session 绑定 TaskManager 后，child 保留自身身份仍可调用继承的 MCP bridge。
/// 历史故障是 bridge 用 child ID 查仅按 root ID 绑定的目录，发送 tools/call 前就失败。
#[tokio::test]
async fn child_mcp_bridge_uses_root_task_owner_for_real_call() {
    let wire = Wire::connect(Source).await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "source", false);
    let manager: Arc<dyn TaskManagerPort> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("root-session", &manager);
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap())
        .with_output_store(&pool, Some("root-session"));
    let context = ToolContext::new(&[], ".")
        .with_session_identity("child-thread", "child-turn")
        .with_mcp_task_owner_session_id("root-session");
    assert_eq!(context.session_id.as_deref(), Some("child-thread"));
    let result = bridge.invoke(serde_json::json!({}), context).await;
    assert!(
        result.is_ok(),
        "child 经真实 MCP tools/call 应成功: {result:?}"
    );
}

/// [回归测试] MCP Tasks 回执沿同一根 owner 登记，不能回落到 child ID。
#[tokio::test]
async fn child_mcp_task_receipt_registers_under_root_owner() {
    let wire = Wire::connect(StartedTask).await;
    let (mut owner, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    wire.install(&pool, "source", false);
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let task_manager: Arc<dyn TaskManagerPort> = manager.clone();
    pool.bind_session_task_manager("root-session", &task_manager);
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap())
        .with_output_store(&pool, Some("root-session"));
    let context = ToolContext::new(&[], ".")
        .with_session_identity("child-thread", "child-turn")
        .with_mcp_task_owner_session_id("root-session");
    let output = bridge.invoke(serde_json::json!({}), context).await.unwrap();
    assert!(output.contains("Background task started:"), "{output}");
    assert!(!manager.snapshot().tasks.is_empty());
    let duplicate = bridge
        .invoke(
            serde_json::json!({}),
            ToolContext::new(&[], ".")
                .with_session_identity("child-thread", "child-turn")
                .with_mcp_task_owner_session_id("root-session"),
        )
        .await
        .unwrap();
    assert_eq!(duplicate, output);
    assert_eq!(owner.active_count(), 1, "同键回执复用已准入 monitor");
    owner.shutdown().await;
    wire.close().await;
}

// 回归：任务已存在但 monitor 准入失败，不再承诺完成通知必达。
#[tokio::test]
async fn test_closed_monitor_owner_returns_honest_task_receipt_error() {
    let wire = Wire::connect(StartedTask).await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "source", false);
    let manager: Arc<dyn TaskManagerPort> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap())
        .with_output_store(&pool, Some("session"));
    let error = bridge
        .invoke(serde_json::json!({}), ToolContext::new(&[], "."))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("monitor was not admitted"), "{error}");
    assert!(
        error.contains("completion delivery is not guaranteed"),
        "{error}"
    );
    assert!(error.contains("do not repeat the command"), "{error}");
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.snapshot().tasks[0].status, "running");
    assert!(!manager.is_execution_idle());
    wire.close().await;
}

// [回归测试] 子会话发起的后台任务：回执承诺的投递必须真的落到发起者，
// 而不是 root。历史故障是回执向发起者承诺、实际按 owner(root) 投递。
#[tokio::test]
async fn child_initiated_task_receipt_delivers_to_the_child_not_root() {
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::session::{MessageQueue, SessionInbox};
    use peri_acp_types::system_reminder::TrustedSystemReminder;
    use peri_acp_types::tasks::TaskTerminalDelivery;

    struct RecordingDelivery {
        delivered: parking_lot::Mutex<Vec<(MessageId, TrustedSystemReminder)>>,
    }
    impl TaskTerminalDelivery for RecordingDelivery {
        fn deliver<'a>(
            &'a self,
            delivery_id: MessageId,
            reminder: &'a TrustedSystemReminder,
            _source: peri_acp_types::session::MessageSource,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.delivered.lock().push((delivery_id, reminder.clone()));
                Ok(())
            })
        }
    }

    let wire = Wire::connect(StartedTask).await;
    let (mut owner, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    wire.install(&pool, "source", false);
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let task_manager: Arc<dyn TaskManagerPort> = manager.clone();
    pool.bind_session_task_manager("root-session", &task_manager);
    let root_inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    peri_acp_types::mcp::McpSubscriptionPort::register_inbox(
        pool.as_ref(),
        "root-session",
        root_inbox.handle(),
    );
    let delivery = Arc::new(RecordingDelivery {
        delivered: parking_lot::Mutex::new(Vec::new()),
    });
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap())
        .with_output_store(&pool, Some("root-session"));
    let context = ToolContext::new(&[], ".")
        .with_session_identity("child-thread", "child-turn")
        .with_mcp_task_owner_session_id("root-session")
        .with_task_terminal_delivery(delivery.clone());
    let output = bridge.invoke(serde_json::json!({}), context).await.unwrap();
    assert!(
        output.contains("committed to the initiating session's transcript"),
        "回执必须只承诺可达的持久投递: {output}"
    );
    let task_id = manager.snapshot().tasks[0].task_id.clone();
    let mut result = peri_acp_types::event::BackgroundTaskResult {
        task_id: task_id.clone(),
        agent_name: "mcp".into(),
        prompt_summary: "source".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    };
    result.task_id = task_id.clone();
    assert!(manager
        .settle_external(&task_id, "terminal-1", result)
        .await
        .unwrap());
    {
        let delivered = delivery.delivered.lock();
        assert_eq!(delivered.len(), 1, "终态提醒必须投给发起者");
        assert_eq!(
            delivered[0].1.as_reminder().metadata["initiator"],
            "child-thread"
        );
        assert_eq!(
            delivered[0].1.as_reminder().metadata["task_owner"],
            "root-session"
        );
    }
    assert!(root_inbox.queue().drain_all().is_empty());
    owner.shutdown().await;
    wire.close().await;
}

fn reference(output: &str, key: &str) -> String {
    let value = output.split_once(key).unwrap().1;
    serde_json::Deserializer::from_str(value)
        .into_iter::<String>()
        .next()
        .unwrap()
        .unwrap()
}

pub(crate) async fn assert_remote_readback(
    peer: &Peer<RoleClient>,
    output: &str,
    expected: &str,
) -> String {
    assert!(output.contains("server=workspace"), "{output}");
    assert!(!output.contains("NOT saved"));
    let uri = reference(output, "resource_uri=");
    let path = reference(output, "path=");
    let resource = peer
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .unwrap();
    let text = serde_json::to_value(resource).unwrap()["contents"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(text, expected);
    let read = peer
        .call_tool(
            CallToolRequestParams::new("Read").with_arguments(
                serde_json::json!({"file_path": path, "offset": 2199, "limit": 10})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert!(!read.is_error.unwrap_or(false));
    assert!(serde_json::to_string(&read)
        .unwrap()
        .contains("remote output 2199"));
    path
}

#[tokio::test]
async fn host_bridge_success_error_and_resource_use_workspace_wire_readback() {
    let host = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let workspace = Wire::connect(peri_mcp_workspace::WorkspaceMcpServer::new(
        remote.path().to_str().unwrap(),
        None,
    ))
    .await;
    let source = Wire::connect(Source).await;
    let pool = Arc::new(McpClientPool::new_empty());
    pool.bind_execution_cwd(host.path()).unwrap();
    let manager: Arc<dyn TaskManagerPort> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);
    workspace.install(&pool, "workspace", true);
    source.install(&pool, "source", false);
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap())
        .with_output_store(&pool, Some("session"));
    for error in [false, true] {
        let input = if error {
            serde_json::json!({"error": true})
        } else {
            serde_json::json!({})
        };
        let result = bridge
            .invoke(input, ToolContext::new(&[], host.path().to_str().unwrap()))
            .await;
        let output = if error {
            result.unwrap_err().to_string()
        } else {
            result.unwrap()
        };
        let saved_path =
            assert_remote_readback(workspace.client.peer(), &output, &large_output()).await;
        assert!(!std::path::Path::new(&reference(&output, "path=")).starts_with(host.path()));
        std::fs::remove_file(saved_path).unwrap();
    }
    let resource = McpResourceTool::new(pool.clone(), Arc::new(McpSkillRegistry::new()))
        .with_session_id("session");
    let output = resource
        .invoke(
            serde_json::json!({"server_name": "source", "uri": "test://large"}),
            ToolContext::new(&[], host.path().to_str().unwrap()),
        )
        .await
        .unwrap();
    let resource_path = assert_remote_readback(
        workspace.client.peer(),
        &output,
        &format!("[text/text/plain]\n{}", large_output()),
    )
    .await;
    let stored_uri = reference(&output, "resource_uri=");
    let readback = resource
        .invoke(
            serde_json::json!({"server_name": "workspace", "uri": stored_uri}),
            ToolContext::new(&[], host.path().to_str().unwrap()),
        )
        .await
        .unwrap();
    assert!(readback.contains("remote output 0 λ"));
    let readback_path = assert_remote_readback(
        workspace.client.peer(),
        &readback,
        &format!("[text/text/plain]\n[text/text/plain]\n{}", large_output()),
    )
    .await;
    std::fs::remove_file(resource_path).unwrap();
    std::fs::remove_file(readback_path).unwrap();
    assert_eq!(std::fs::read_dir(host.path()).unwrap().count(), 0);
    pool.clients.write().clear();
    source.close().await;
    workspace.close().await;
}

#[tokio::test]
async fn unconfigured_bridge_and_missing_workspace_do_not_claim_saved_output() {
    let source = Wire::connect(Source).await;
    let pool = Arc::new(McpClientPool::new_empty());
    source.install(&pool, "source", false);
    let bridge = McpToolBridge::new("source", &test_tool(), pool.get_client("source").unwrap());
    for error in [false, true] {
        let input = if error {
            serde_json::json!({"error": true})
        } else {
            serde_json::json!({})
        };
        let result = bridge.invoke(input, ToolContext::new(&[], ".")).await;
        let output = if error {
            result.unwrap_err().to_string()
        } else {
            result.unwrap()
        };
        assert!(output.contains("Full output NOT saved"));
        assert!(!output.contains("resource_uri="));
    }
    let resource = McpResourceTool::new(pool.clone(), Arc::new(McpSkillRegistry::new()));
    let output = resource
        .invoke(
            serde_json::json!({"server_name": "source", "uri": "test://large"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(output.contains("Full output NOT saved"));
    pool.clients.write().clear();
    source.close().await;
}

struct DelayedStore {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cancelled: Arc<Notify>,
    invalid: bool,
}

impl ServerHandler for DelayedStore {
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, rmcp::ErrorData> {
        assert_eq!(request.method, STORE_OUTPUT_METHOD);
        let input: StoreOutputRequest = serde_json::from_value(request.params.unwrap()).unwrap();
        self.entered.notify_one();
        tokio::select! {
            _ = context.ct.cancelled() => {
                self.cancelled.notify_one();
                Err(rmcp::ErrorData::internal_error("cancelled", None))
            }
            _ = self.release.notified() => Ok(CustomResult::new(serde_json::json!({
                "uri": if self.invalid { "file:///host/output".into() } else {
                    format!("peri-output://{}/{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
                },
                "path": "/remote/tool/output.txt", "byte_length": input.content.len()
            }))),
        }
    }
}

struct DelayedFixture {
    pool: Arc<McpClientPool>,
    wire: Wire,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cancelled: Arc<Notify>,
}

impl DelayedFixture {
    async fn new(invalid: bool) -> Self {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let wire = Wire::connect(DelayedStore {
            entered: entered.clone(),
            release: release.clone(),
            cancelled: cancelled.clone(),
            invalid,
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty());
        wire.install(&pool, "workspace", true);
        Self {
            pool,
            wire,
            entered,
            release,
            cancelled,
        }
    }

    fn store(
        &self,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Result<StoredOutput, &'static str>> {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            pool.store_output_with_timeout(Some("session"), "full remote output", timeout)
                .await
        })
    }

    async fn close(self) {
        self.pool.clients.write().clear();
        self.wire.close().await;
    }
}

#[tokio::test]
async fn store_rejects_closed_replaced_regenerated_and_reowned_workspace_results() {
    for change in ["close", "replace", "generation", "owner", "disabled"] {
        let fixture = DelayedFixture::new(false).await;
        let task = fixture.store(Duration::from_secs(2));
        tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
            .await
            .unwrap();
        let current = fixture.pool.get_client("workspace").unwrap();
        match change {
            "close" => fixture.pool.begin_shutdown(),
            "replace" => {
                fixture
                    .pool
                    .clients
                    .write()
                    .insert("workspace".into(), Arc::new((*current).clone()));
            }
            "generation" => {
                fixture.pool.advance_handle_generation(&current);
            }
            "owner" => {
                fixture
                    .pool
                    .acp_owners
                    .write()
                    .insert("workspace".into(), "session".into());
            }
            "disabled" => {
                fixture
                    .pool
                    .set_builtin_instance_context(Arc::new(
                        crate::mcp::builtin::context::BuiltinInstanceContext::new("/remote")
                            .with_closed(std::collections::BTreeSet::from(["workspace".into()])),
                    ))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        fixture.release.notify_one();
        assert!(task.await.unwrap().is_err(), "{change}");
        fixture.close().await;
    }
}

#[tokio::test]
async fn output_timeout_and_drop_notify_server_cancellation() {
    for drop_request in [false, true] {
        let fixture = DelayedFixture::new(false).await;
        let task = fixture.store(Duration::from_millis(100));
        tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
            .await
            .unwrap();
        if drop_request {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(task.await.unwrap().unwrap_err(), "output store timed out");
        }
        tokio::time::timeout(Duration::from_secs(2), fixture.cancelled.notified())
            .await
            .unwrap();
        fixture.close().await;
    }
}

#[tokio::test]
async fn invalid_receipt_and_foreign_owner_are_not_saved_success() {
    let fixture = DelayedFixture::new(true).await;
    fixture.release.notify_one();
    let output = format_output(Some(&fixture.pool), Some("session"), large_output(), false).await;
    assert!(output.contains("Full output NOT saved: invalid output store response"));
    fixture
        .pool
        .acp_owners
        .write()
        .insert("workspace".into(), "other-session".into());
    assert!(fixture
        .pool
        .store_output(Some("session"), "secret")
        .await
        .is_err());
    fixture.close().await;
}

#[tokio::test]
async fn rpc_failure_never_claims_saved_output() {
    let wire = Wire::connect(Source).await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "workspace", true);
    let output = format_output(Some(&pool), Some("session"), large_output(), false).await;
    assert!(output.contains("Full output NOT saved: output store RPC failed"));
    assert!(!output.contains("resource_uri="));
    pool.clients.write().clear();
    wire.close().await;
}
