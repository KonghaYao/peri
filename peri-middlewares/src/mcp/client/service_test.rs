use super::*;
use rmcp::{
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{async_rw::AsyncRwTransport, Transport},
};
use std::{future::Future, io, time::Duration};
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
    let service = super::super::transport::serve_client_auto(
        transport,
        None,
        None,
        &profile,
        Duration::from_secs(5),
    )
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
