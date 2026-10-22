//! Exercise control through real session admission and the host receive loop.
use super::*;
use peri_acp_types::{
    mcp_control::{McpControlRequest, McpControlResponse, McpControlStatus},
    ports::{McpPoolPort, McpPoolShutdownReport},
};
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

#[derive(Default)]
struct BlockingPool {
    calls: AtomicUsize,
    list_calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl McpPoolPort for BlockingPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn snapshot(&self) -> Value {
        json!({})
    }
    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }
    fn server_infos(&self) -> Result<Vec<peri_acp_types::ports::McpServerInfo>, String> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    async fn control_mcp(
        self: Arc<Self>,
        request: McpControlRequest,
    ) -> Result<McpControlResponse, peri_acp_types::mcp_control::McpControlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(McpControlResponse {
            session_id: request.session_id().into(),
            server_name: request.server_name().into(),
            action: request.action(),
            status: McpControlStatus::Stopped,
        })
    }
}

async fn fixture(
    tmp: &tempfile::TempDir,
) -> (
    AcpServerConfig,
    HashMap<String, SessionState>,
    String,
    Arc<BlockingPool>,
) {
    let config = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "synthetic-key",
        "model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: tmp
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd":tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let pool = Arc::new(BlockingPool::default());
    let env = Arc::get_mut(sessions.get_mut(&id).unwrap().environment.as_mut().unwrap()).unwrap();
    // Replace the session's backend to control completion timing. Production
    // assembly freezes the policy; this fixture explicitly exercises enabled control.
    env.cfg.mcp_pool = Some(pool.clone());
    env.mcp_control_enabled = true;
    (cfg, sessions, id, pool)
}
fn negotiated() -> crate::host::connection::ConnectionFacts {
    let mut connection = crate::host::connection::ConnectionContext::new(false);
    connection.commit_initialize(PeriCaps {
        mcp_control_v1: true,
        ..PeriCaps::default()
    });
    connection.facts()
}

#[tokio::test]
#[serial]
async fn control_rejects_unnegotiated_invalid_unknown_and_closing_without_backend_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, mut sessions, id, pool) = fixture(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let request = json!({"sessionId":id,"action":"stop","serverName":"plugin:example:docs"});
    let failure = handle_request(
        "peri/mcp/control",
        &request,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(failure.data.unwrap()["kind"], "capability_not_negotiated");
    // The historical display fallback contains OAuth/agentEvent, but it must
    // never authorize pre-initialize MCP management or leak backend projections.
    let mut closed = crate::host::connection::ConnectionContext::new(false);
    closed.commit_initialize(PeriCaps::all_enabled());
    closed.begin_close();
    for connection in [
        crate::host::connection::ConnectionFacts::unnegotiated(),
        closed.facts(),
    ] {
        let failure = handle_request_for_connection(
            "mcp/list",
            &json!({"sessionId":id}),
            &cfg,
            &mut sessions,
            &transport,
            connection,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, -32601);
        assert_eq!(pool.list_calls.load(Ordering::SeqCst), 0);
    }
    for (params, kind) in [
        (
            json!({"sessionId":id,"action":"stop","serverName":"docs","config":{}}),
            "invalid_request",
        ),
        (
            json!({"sessionId":"unknown","action":"stop","serverName":"docs"}),
            "session_not_found",
        ),
    ] {
        let failure = handle_request_for_connection(
            "peri/mcp/control",
            &params,
            &cfg,
            &mut sessions,
            &transport,
            negotiated(),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.data.unwrap()["kind"], kind);
    }
    let failure = handle_request_for_connection(
        "mcp/list",
        &json!({"sessionId":"unknown"}),
        &cfg,
        &mut sessions,
        &transport,
        negotiated(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.data.unwrap()["kind"], "session_not_found");
    sessions.get_mut(&id).unwrap().closing = true;
    let failure = handle_request_for_connection(
        "peri/mcp/control",
        &request,
        &cfg,
        &mut sessions,
        &transport,
        negotiated(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.data.unwrap()["kind"], "session_closed");
    assert_eq!(pool.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pool.list_calls.load(Ordering::SeqCst), 0);
    sessions.get_mut(&id).unwrap().closing = false;
    assert!(sessions[&id].environment.as_ref().unwrap().shutdown().await);
}

struct ScriptedTransport {
    incoming: std::sync::Mutex<VecDeque<IncomingMessage>>,
    responses: std::sync::Mutex<Vec<(RequestId, Result<Value, AcpError>)>>,
    response_ready: tokio::sync::Notify,
}
#[async_trait]
impl crate::transport::AcpTransport for ScriptedTransport {
    async fn send_request(&self, _: &str, _: Value) -> Result<Value, AcpError> {
        Ok(Value::Null)
    }
    async fn send_notification(&self, _: &str, _: Value) -> Result<(), AcpError> {
        Ok(())
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming.lock().unwrap().pop_front()
    }
    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.responses.lock().unwrap().push((id, result));
        self.response_ready.notify_one();
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn slow_control_keeps_server_loop_and_new_capability_list_responsive() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, sessions, id, pool) = fixture(&tmp).await;
    let cfg = Arc::new(cfg);
    let sessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let transport = Arc::new(ScriptedTransport {
        incoming: std::sync::Mutex::new(VecDeque::from([
            IncomingMessage::Request {
                id: RequestId::Number(1),
                method: "initialize".into(),
                params: json!({"protocolVersion":1,"clientCapabilities":{"_meta":{"peri.mcpControlV1":true}}}),
            },
            IncomingMessage::Request {
                id: RequestId::Number(2),
                method: "peri/mcp/control".into(),
                params: json!({"sessionId":id,"action":"stop","serverName":"plugin:example:docs"}),
            },
            IncomingMessage::Request {
                id: RequestId::Number(3),
                method: "mcp/list".into(),
                params: json!({"sessionId":id}),
            },
        ])),
        responses: Default::default(),
        response_ready: Default::default(),
    });
    let erased: Arc<dyn crate::transport::AcpTransport> = transport.clone();
    let prompt_locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = Arc::new(tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(false),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    crate::host::server_loop::ServerLoop {
        transport: &erased,
        cfg: &cfg,
        sessions: &sessions,
        prompt_locks: &prompt_locks,
        cont_tx: &tx,
        connection: &connection,
        connection_cancellation: &cancellation,
    }
    .run()
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), pool.entered.notified())
        .await
        .unwrap();
    {
        let responses = transport.responses.lock().unwrap();
        assert!(responses
            .iter()
            .any(|(key, result)| *key == RequestId::Number(3)
                && result.as_ref().unwrap()["servers"] == json!([])));
        assert!(!responses
            .iter()
            .any(|(key, _)| *key == RequestId::Number(2)));
        let init = responses
            .iter()
            .find(|(key, _)| *key == RequestId::Number(1))
            .unwrap()
            .1
            .as_ref()
            .unwrap();
        assert_eq!(
            init["agentCapabilities"]["_meta"]["peri.mcpControlV1"],
            true
        );
    }
    pool.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ready = transport.response_ready.notified();
            if transport
                .responses
                .lock()
                .unwrap()
                .iter()
                .any(|(key, _)| *key == RequestId::Number(2))
            {
                break;
            }
            ready.await;
        }
    })
    .await
    .unwrap();
    assert_eq!(pool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(pool.list_calls.load(Ordering::SeqCst), 1);
    assert!(
        sessions.lock().await[&id]
            .environment
            .as_ref()
            .unwrap()
            .shutdown()
            .await
    );
}
