use super::{Arc, McpServiceWrapper, RoleClient};
use rmcp::service::RxJsonRpcMessage;
use rmcp::service::TxJsonRpcMessage;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::Transport;
use std::future::Future;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct DelayedClose {
    io: AsyncRwTransport<RoleClient, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>,
    release: Arc<tokio::sync::Notify>,
}

impl Transport<RoleClient> for DelayedClose {
    type Error = io::Error;
    fn send(
        &mut self,
        message: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.io.send(message)
    }
    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.io.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        self.release.notified().await;
        self.io.close().await
    }
}

async fn delayed_service() -> (
    McpServiceWrapper,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let (client, server) = tokio::io::duplex(4096);
    let (server_read, mut server_write) = tokio::io::split(server);
    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if request["method"] == "server/discover" {
                let response = serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32601, "message": "Method not found" }
                });
                server_write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
                server_write.flush().await.unwrap();
            }
            if request["method"] == "initialize" {
                let response = serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"], "result": {
                        "protocolVersion": "2025-11-25", "capabilities": {},
                        "serverInfo": { "name": "drain-fixture", "version": "1" }
                    }
                });
                server_write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
                server_write.flush().await.unwrap();
            }
        }
    });
    let (read, write) = tokio::io::split(client);
    let release = Arc::new(tokio::sync::Notify::new());
    let transport = DelayedClose {
        io: AsyncRwTransport::new(read, write),
        release: release.clone(),
    };
    let profile = crate::mcp::apps::McpCapabilityProfile::default();
    let service =
        super::super::transport::serve_client_auto(transport, &profile, Duration::from_secs(5))
            .await
            .unwrap()
            .unwrap();
    (service, release, server)
}

#[tokio::test]
async fn timed_out_and_cancelled_sdk_close_retries_the_original_transport_drain() {
    let (mut service, release, server) = delayed_service().await;
    assert!(service
        .close_with_timeout(Duration::from_millis(20))
        .await
        .unwrap()
        .is_none());
    assert!(tokio::time::timeout(
        Duration::from_millis(20),
        service.close_with_timeout(Duration::from_secs(5)),
    )
    .await
    .is_err());
    release.notify_one();
    assert!(service
        .close_with_timeout(Duration::from_secs(5))
        .await
        .unwrap()
        .is_some());
    server.await.unwrap();
}

#[tokio::test]
async fn pool_retains_protocol_owner_after_its_handle_and_close_waiter_are_dropped() {
    let (service, release, server) = delayed_service().await;
    let pool = super::super::McpClientPool::new_pending();
    let mut service = pool.retain_service(service);
    assert!(service
        .close_with_timeout(Duration::from_millis(20))
        .await
        .unwrap()
        .is_none());
    drop(service);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), pool.shutdown())
            .await
            .is_err()
    );
    assert!(matches!(
        pool.shutdown().await,
        peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
            unfinished_services: 1,
            ..
        }
    ));
    release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pool.shutdown())
            .await
            .unwrap()
            .is_complete()
    );
    server.await.unwrap();
}

#[test]
#[allow(deprecated)]
fn client_info_declares_url_elicitation_but_not_form() {
    let info =
        super::mcpp_client_info_for_profile(&crate::mcp::apps::McpCapabilityProfile::default());
    let elicitation = info
        .capabilities
        .elicitation
        .as_ref()
        .expect("URL elicitation capability must be declared");
    assert!(elicitation.url.is_some(), "URL 模式必须声明");
    assert!(elicitation.form.is_none(), "form 模式不在支持范围");
}

/// 线上 `mode: "url"` 必须解析为 URL 变体（否则 handler 会走 form / 兜底分支）。
#[test]
#[allow(deprecated)]
fn url_mode_params_deserialize_as_url_elicitation() {
    let params: super::ElicitRequestParams = serde_json::from_value(serde_json::json!({
        "mode": "url",
        "message": "Authorize the browser session",
        "url": "https://example.test/authorize",
        "elicitationId": "e-1"
    }))
    .expect("URL elicitation params 必须可解析");
    let is_url = matches!(
        &params,
        super::ElicitRequestParams::UrlElicitationParams {
            url,
            elicitation_id,
            ..
        } if url == "https://example.test/authorize" && elicitation_id == "e-1"
    );
    assert!(is_url, "实际解析为 {params:?}");
}

/// 记录 URL elicitation 投递内容、回固定决策的宿主端口替身。
struct RecordingElicitationPort {
    calls: parking_lot::Mutex<Vec<super::super::UrlElicitationRequest>>,
    outcome: super::super::UrlElicitationOutcome,
}

#[async_trait::async_trait]
impl super::super::UrlElicitationPort for RecordingElicitationPort {
    async fn elicit_url(
        &self,
        request: super::super::UrlElicitationRequest,
    ) -> super::super::UrlElicitationOutcome {
        self.calls.lock().push(request);
        self.outcome
    }
}

type ElicitationFixture = (
    Arc<super::super::McpClientPool>,
    McpServiceWrapper,
    Arc<RecordingElicitationPort>,
    tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    Arc<parking_lot::Mutex<Option<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
);

/// 假 server：完成 Auto 握手后连发一条 URL 模式与一条 form 模式的
/// `elicitation/create`，并把客户端响应转给测试。
async fn elicitation_fixture() -> ElicitationFixture {
    let (client, server) = tokio::io::duplex(8192);
    let (server_read, mut server_write) = tokio::io::split(server);
    let (responses, responses_rx) = tokio::sync::mpsc::unbounded_channel();
    let initialize = Arc::new(parking_lot::Mutex::new(None));
    let captured_initialize = Arc::clone(&initialize);
    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let mut elicitations_sent = false;
        while let Some(line) = lines.next_line().await.unwrap() {
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            match message["method"].as_str() {
                Some("server/discover") => {
                    let response = serde_json::json!({
                        "jsonrpc": "2.0", "id": message["id"],
                        "error": { "code": -32601, "message": "Method not found" }
                    });
                    server_write
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .unwrap();
                    server_write.flush().await.unwrap();
                }
                Some("initialize") => {
                    *captured_initialize.lock() = Some(message["params"].clone());
                    let response = serde_json::json!({
                        "jsonrpc": "2.0", "id": message["id"], "result": {
                            "protocolVersion": "2025-11-25", "capabilities": {},
                            "serverInfo": { "name": "elicitation-fixture", "version": "1" }
                        }
                    });
                    server_write
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .unwrap();
                    server_write.flush().await.unwrap();
                    if !elicitations_sent {
                        elicitations_sent = true;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        for request in [
                            serde_json::json!({
                                "jsonrpc": "2.0", "id": 100, "method": "elicitation/create",
                                "params": {
                                    "mode": "url",
                                    "message": "Authorize the browser session",
                                    "url": "https://example.test/authorize",
                                    "elicitationId": "e-1"
                                }
                            }),
                            serde_json::json!({
                                "jsonrpc": "2.0", "id": 101, "method": "elicitation/create",
                                "params": {
                                    "mode": "form",
                                    "message": "What colour?",
                                    "requestedSchema": { "type": "object", "properties": {} }
                                }
                            }),
                        ] {
                            server_write
                                .write_all(format!("{request}\n").as_bytes())
                                .await
                                .unwrap();
                            server_write.flush().await.unwrap();
                        }
                    }
                }
                _ => {
                    if matches!(message["id"].as_u64(), Some(100 | 101)) {
                        let _ = responses.send(message);
                    }
                }
            }
        }
    });
    let (read, write) = tokio::io::split(client);
    let pool = Arc::new(super::super::McpClientPool::new_pending());
    let port = Arc::new(RecordingElicitationPort {
        calls: parking_lot::Mutex::new(Vec::new()),
        outcome: super::super::UrlElicitationOutcome::Accepted,
    });
    pool.set_url_elicitation_port(port.clone());
    let service = super::super::transport::serve_client_auto_for_server(
        AsyncRwTransport::new(read, write),
        &pool,
        "peri-browser",
        Duration::from_secs(5),
    )
    .await
    .unwrap()
    .unwrap();
    (pool, service, port, responses_rx, initialize, server)
}

#[tokio::test]
async fn url_elicitation_is_forwarded_to_the_host_port_and_accepted() {
    // `_pool` 必须活到断言结束：handler 只持弱引用，池释放即按 decline 处理。
    let (_pool, _service, port, mut responses, initialize, server) = elicitation_fixture().await;

    let declared = initialize.lock().clone().expect("initialize 未被捕获");
    assert!(
        declared["capabilities"]["elicitation"]["url"].is_object(),
        "initialize 必须声明 URL elicitation：{declared}"
    );
    assert!(declared["capabilities"]["elicitation"]
        .get("form")
        .is_none());

    let accepted = tokio::time::timeout(Duration::from_secs(5), responses.recv())
        .await
        .unwrap()
        .unwrap();
    let calls = port.calls.lock().clone();
    assert_eq!(
        (
            accepted["id"].as_u64(),
            accepted["result"]["action"].as_str(),
            calls.len()
        ),
        (Some(100), Some("accept"), 1),
        "response = {accepted}, calls = {calls:?}"
    );
    assert_eq!(calls[0].server_name, "peri-browser");
    assert_eq!(calls[0].message, "Authorize the browser session");
    assert_eq!(calls[0].url, "https://example.test/authorize");
    assert_eq!(calls[0].elicitation_id, "e-1");

    let declined = tokio::time::timeout(Duration::from_secs(5), responses.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(declined["id"], 101);
    assert_eq!(declined["result"]["action"], "decline");
    assert_eq!(port.calls.lock().len(), 1, "form 模式不得进入宿主端口");

    server.abort();
}
