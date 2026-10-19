use super::*;
use peri_acp_types::{
    ports::McpPoolShutdownReport,
    tasks::{BgTaskKind, BgTaskRegistration},
    workflow::{AgentExecutor, AgentRunParams, AgentRunResult, WorkflowRunStatus},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Executor;
#[async_trait::async_trait]
impl AgentExecutor for Executor {
    async fn execute(&self, _: AgentRunParams) -> AgentRunResult {
        panic!("projection must not execute agents")
    }
}

struct WirePool {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    disconnected: AtomicBool,
}
#[async_trait::async_trait]
impl McpPoolPort for WirePool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn snapshot(&self) -> serde_json::Value {
        json!({})
    }
    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }
    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        assert_eq!(instance, "workflow");
        if self.disconnected.load(Ordering::SeqCst) {
            return Err("wire disconnected".into());
        }
        let result = self
            .peer
            .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
            .await
            .map_err(|error| error.to_string())?;
        match &result.contents[0] {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Ok(text.clone()),
            _ => Err("not text".into()),
        }
    }
    async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> Result<String, String> {
        assert_eq!(instance, "workflow");
        if self.disconnected.load(Ordering::SeqCst) {
            return Err("wire disconnected".into());
        }
        let mut params = rmcp::model::CallToolRequestParams::new(tool.to_owned())
            .with_arguments(arguments.as_object().unwrap().clone());
        params.meta = meta.map(|value| {
            rmcp::model::RequestMetaObject(rmcp::model::MetaObject(
                value.as_object().unwrap().clone(),
            ))
        });
        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|error| error.to_string())?;
        let text = result
            .content
            .iter()
            .filter_map(|content| content.as_text())
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

#[tokio::test]
async fn workflow_projection_requires_wire_and_acknowledges_only_accepted_terminal() {
    let (tx, _) = tokio::sync::broadcast::channel(1);
    let runtime = Arc::new(peri_mcp_workflow::WorkflowRuntime::new(
        Arc::new(Executor),
        "/tmp",
        tx,
        None,
    ));
    let token = runtime.bind_consumer_token();
    let input = peri_mcp_workflow::WorkflowInstanceInput::new(runtime.clone());
    let owner = runtime.bg_registry().unwrap();
    let host: Arc<dyn TaskManager> = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    assert!(!Arc::ptr_eq(&owner, &host));
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        rmcp::serve_server(
            peri_mcp_workflow::WorkflowMcpServer::new(input),
            tokio::io::split(server_io),
        )
        .await
        .unwrap()
        .waiting()
        .await
        .unwrap();
    });
    let mut service = rmcp::serve_client_with_lifecycle(
        (),
        tokio::io::split(client_io),
        rmcp::ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        },
    )
    .await
    .unwrap();
    let pool = Arc::new(WirePool {
        peer: service.peer().clone(),
        disconnected: AtomicBool::new(false),
    });
    let accepted = Arc::new(AtomicUsize::new(0));
    let reject = Arc::new(AtomicBool::new(true));
    let delivered = accepted.clone();
    let reject_delivery = reject.clone();
    let host_for_delivery = host.clone();
    let client = Client {
        pool: pool.clone(),
        manager: host.clone(),
        session_id: "session-a".into(),
        token,
        deliver: Arc::new(move |_| {
            if reject_delivery.load(Ordering::SeqCst) {
                return Err("mailbox unavailable".into());
            }
            assert_eq!(
                host_for_delivery.active_count(),
                1,
                "terminal delivery precedes active-count release"
            );
            delivered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
        scan: tokio::sync::Mutex::new(()),
    };
    owner
        .register(BgTaskRegistration {
            task_id: "run-a".into(),
            kind: BgTaskKind::Workflow,
            summary: "wire run".into(),
            pid: None,
            kill: None,
        })
        .unwrap();
    assert!(
        host.snapshot().tasks.is_empty(),
        "owner registration must not leak through shared memory"
    );
    client.reconcile().await.unwrap();
    assert_eq!(host.active_count(), 1);
    pool.disconnected.store(true, Ordering::SeqCst);
    let terminal = WorkflowTaskResult {
        run_id: "run-a".into(),
        workflow_name: "wire run".into(),
        success: false,
        status: WorkflowRunStatus::Failed,
        execution_status: Default::default(),
        acceptance_status: Default::default(),
        post_processing_status: Default::default(),
        delivery_status: Default::default(),
        state_artifact_exists: false,
        duration_ms: 10,
        agent_count: 0,
        tool_calls_count: 0,
        error: Some("failed".into()),
        phase_summaries: vec![],
        attempts: vec![],
    };
    runtime.registry().complete("run-a", terminal.clone());
    owner.confirm_external_execution_stopped("run-a");
    owner
        .settle_completed(
            "run-a",
            terminal.background_result(),
            Arc::new(|_, _| Ok(())),
        )
        .unwrap();
    assert!(client.reconcile().await.is_err());
    assert_eq!(host.active_count(), 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 0);
    pool.disconnected.store(false, Ordering::SeqCst);
    assert!(
        client.reconcile().await.is_err(),
        "unaccepted delivery must retain the owner outbox"
    );
    assert_eq!(runtime.registry().pending_completions().len(), 1);
    reject.store(false, Ordering::SeqCst);
    client.reconcile().await.unwrap();
    assert!(runtime.registry().pending_completions().is_empty());
    assert_eq!(host.active_count(), 0);
    client.reconcile().await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    client.close().await.unwrap();
    let _ = service.close_with_timeout(Duration::from_secs(1)).await;
    server.abort();
}

#[tokio::test]
#[serial_test::serial]
async fn workflow_real_engine_start_ack_cancel_and_close_cross_mcp_wire() {
    if std::env::var_os("PERI_WORKFLOW_ARTIFACT_DIR").is_none() {
        eprintln!("real engine test requires PERI_WORKFLOW_ARTIFACT_DIR");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (tx, _) = tokio::sync::broadcast::channel(1);
    let runtime = Arc::new(peri_mcp_workflow::WorkflowRuntime::new(
        Arc::new(Executor),
        dir.path().to_str().unwrap(),
        tx,
        None,
    ));
    let token = runtime.bind_consumer_token();
    let input = peri_mcp_workflow::WorkflowInstanceInput::new(runtime.clone());
    runtime.set_consumer_ready(true);
    let owner = runtime.bg_registry().unwrap();
    let host: Arc<dyn TaskManager> = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        rmcp::serve_server(
            peri_mcp_workflow::WorkflowMcpServer::new(input),
            tokio::io::split(server_io),
        )
        .await
        .unwrap()
        .waiting()
        .await
        .unwrap();
    });
    let mut service = rmcp::serve_client_with_lifecycle(
        (),
        tokio::io::split(client_io),
        rmcp::ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        },
    )
    .await
    .unwrap();
    let pool = Arc::new(WirePool {
        peer: service.peer().clone(),
        disconnected: AtomicBool::new(false),
    });
    let accepted = Arc::new(AtomicUsize::new(0));
    let delivered = accepted.clone();
    let client = Client {
        pool: pool.clone(),
        manager: host.clone(),
        session_id: "engine-session".into(),
        token,
        deliver: Arc::new(move |_| {
            delivered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
        scan: tokio::sync::Mutex::new(()),
    };
    let starter = pool.clone();
    let mut started = tokio::spawn(async move {
        starter.call_instance_tool("workflow", "Workflow", json!({"script":"export const meta = { name: 'mcp-boundary', description: 'MCP boundary regression' }; const result = await agent('wait for consumer'); return { result };"}), None).await
    });
    let early = tokio::time::timeout(Duration::from_millis(1500), &mut started).await;
    assert!(
        early.is_err(),
        "background response must wait for the Host projection receipt; early result: {early:?}"
    );
    assert_eq!(owner.active_count(), 1);
    assert_eq!(host.active_count(), 0);
    client.reconcile().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), started)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(host.active_count(), 1);
    let task_id = host.snapshot().tasks[0].task_id.clone();
    host.cancel_async(&task_id).await.unwrap();
    assert_eq!(
        runtime.registry().active_count(),
        0,
        "Host cancellation must reach WorkflowKill over MCP"
    );
    client.close().await.unwrap();
    assert!(
        owner.is_execution_idle(),
        "successful close must prove actual engine cleanup"
    );
    assert_eq!(host.active_count(), 0);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    let _ = service.close_with_timeout(Duration::from_secs(1)).await;
    server.abort();
}
