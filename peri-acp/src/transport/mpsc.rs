//! In-memory ACP transport using tokio mpsc channels.
//!
//! `mpsc_transport_pair()` creates a connected pair of transports — one for the
//! ACP server side and one for the client (TUI) side. Messages flow through two
//! pairs of bounded channels; a peer that stops draining is closed after a
//! bounded wait instead of growing memory without limit.
//!
//! Each transport spawns a background pump task that continuously reads incoming
//! messages and dispatches responses to the pending request map, so `send_request`
//! can await the oneshot channel without deadlocking.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    router::{transport_closed_error, RequestRouter},
    types::{AcpError, IncomingMessage, RequestId},
    AcpTransport,
};

/// 进程内传输队列容量。内容与控制共用同一条 FIFO；满时写端只等待有限时间，
/// 超时后显式关闭连接，避免慢消费者把无界内存增长伪装成“还在运行”。
const MPSC_QUEUE_CAPACITY: usize = 1024;

/// 写端等待上限。超时视为对端停滞：关闭连接并返回明确的背压错误，
/// 不静默丢帧，也不无限 await（取消/关闭必须仍能推进）。
const MPSC_SEND_TIMEOUT: Duration = Duration::from_millis(500);

/// JSON-RPC server error code（-32000..=-32099）用于表达“传输层背压”，
/// 与 host 的 turn execution failure 共用同一段保留码但语义由 message 区分。
const TRANSPORT_BACKPRESSURE_CODE: i64 = -32000;

// ---------- internal channel message types ----------

#[derive(Debug)]
enum ChannelMessage {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        result: Result<Value, AcpError>,
    },
}

// ---------- shared pending map ----------

/// Convert an internal `ChannelMessage` into a public `IncomingMessage` for dispatch.
fn channel_to_incoming(msg: ChannelMessage) -> IncomingMessage {
    match msg {
        ChannelMessage::Request { id, method, params } => {
            IncomingMessage::Request { id, method, params }
        }
        ChannelMessage::Notification { method, params } => {
            IncomingMessage::Notification { method, params }
        }
        ChannelMessage::Response { id, result } => IncomingMessage::Response { id, result },
    }
}

fn spawn_pump(
    mut receiver: mpsc::Receiver<ChannelMessage>,
    router: RequestRouter,
) -> mpsc::Receiver<IncomingMessage> {
    let (incoming_tx, incoming_rx) = mpsc::channel(MPSC_QUEUE_CAPACITY);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                message = receiver.recv() => {
                    let Some(message) = message else {
                        router.close();
                        break;
                    };
                    let incoming = channel_to_incoming(message);
                    if router.dispatch(&incoming) {
                        continue;
                    }
                    // 接收侧不读时有限等待：超时关闭连接，避免 pump 永久卡在 send
                    // 上导致关闭信号也无法推进。这里不丢帧后继续运行——连接已经
                    // 不可信，统一按显式关闭处理。
                    tokio::select! {
                        result = incoming_tx.send(incoming) => {
                            if result.is_err() {
                                router.close();
                                break;
                            }
                        }
                        () = router.wait_closed() => break,
                        () = tokio::time::sleep(MPSC_SEND_TIMEOUT) => {
                            tracing::warn!(
                                event = "acp_transport_backpressure",
                                "进程内 ACP 传输接收侧未在期限内消费，关闭连接"
                            );
                            router.close();
                            break;
                        }
                    }
                }
                () = router.wait_closed() => break,
            }
        }
    });
    incoming_rx
}

async fn send_or_close(
    sender: &mpsc::Sender<ChannelMessage>,
    router: &RequestRouter,
    message: ChannelMessage,
) -> Result<(), AcpError> {
    router.ensure_open()?;
    match tokio::time::timeout(MPSC_SEND_TIMEOUT, sender.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => {
            router.close();
            Err(transport_closed_error())
        }
        Err(_) => {
            tracing::warn!(
                event = "acp_transport_backpressure",
                "进程内 ACP 传输队列已满，写端等待超时，关闭连接"
            );
            router.close();
            Err(AcpError::new(
                TRANSPORT_BACKPRESSURE_CODE,
                "ACP transport backpressure: peer is not draining messages",
            ))
        }
    }
}

// ---------- MpscClientTransport ----------

/// Client-side (TUI) transport.
pub struct MpscClientTransport {
    /// Sends client → server messages.
    client_tx: mpsc::Sender<ChannelMessage>,
    /// Receives processed incoming messages from the pump.
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<IncomingMessage>>,
    /// Shared request-response router.
    router: RequestRouter,
}

impl MpscClientTransport {
    /// 显式关闭两端共享 router，使保留 transport Arc 的 pump 和 pending 请求退出。
    pub fn close(&self) {
        self.router.close();
    }

    fn new(
        client_tx: mpsc::Sender<ChannelMessage>,
        server_rx: mpsc::Receiver<ChannelMessage>,
        router: RequestRouter,
    ) -> Self {
        let incoming_rx = spawn_pump(server_rx, router.clone());

        Self {
            client_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
        }
    }
}

#[async_trait]
impl AcpTransport for MpscClientTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.send_cancellable_request(method, params, tokio_util::sync::CancellationToken::new())
            .await
    }

    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Value, AcpError> {
        let pending = self.router.register()?;
        let id = RequestId::Number(pending.id());

        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            },
        )
        .await?;

        super::wait_cancellable(self, pending, cancellation).await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Notification {
                method: method.to_string(),
                params,
            },
        )
        .await
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        send_or_close(
            &self.client_tx,
            &self.router,
            ChannelMessage::Response { id, result },
        )
        .await
    }
}

// ---------- MpscServerTransport ----------

/// Server-side (ACP) transport.
pub struct MpscServerTransport {
    /// Sends server → client messages.
    server_tx: mpsc::Sender<ChannelMessage>,
    /// Receives processed incoming messages from the pump.
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<IncomingMessage>>,
    /// Shared request-response router.
    router: RequestRouter,
}

impl MpscServerTransport {
    fn new(
        client_rx: mpsc::Receiver<ChannelMessage>,
        server_tx: mpsc::Sender<ChannelMessage>,
        router: RequestRouter,
    ) -> Self {
        let incoming_rx = spawn_pump(client_rx, router.clone());

        Self {
            server_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
        }
    }
}

#[async_trait]
impl AcpTransport for MpscServerTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.send_cancellable_request(method, params, tokio_util::sync::CancellationToken::new())
            .await
    }

    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Value, AcpError> {
        let pending = self.router.register()?;
        let id = RequestId::Number(pending.id());

        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Request {
                id,
                method: method.to_string(),
                params,
            },
        )
        .await?;

        super::wait_cancellable(self, pending, cancellation).await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Notification {
                method: method.to_string(),
                params,
            },
        )
        .await
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        send_or_close(
            &self.server_tx,
            &self.router,
            ChannelMessage::Response { id, result },
        )
        .await
    }
}

// ---------- factory ----------

/// Create a connected pair of in-memory ACP transports.
///
/// Returns `(client, server)` where:
/// - `client` is used by the TUI / ACP client side
/// - `server` is used by the ACP session manager side
///
/// Each transport spawns a background pump task for processing incoming
/// messages, so the pair must be created within a tokio runtime.
pub fn mpsc_transport_pair() -> (MpscClientTransport, MpscServerTransport) {
    let (client_tx, client_rx) = mpsc::channel(MPSC_QUEUE_CAPACITY);
    let (server_tx, server_rx) = mpsc::channel(MPSC_QUEUE_CAPACITY);

    let client_router = RequestRouter::new();
    let server_router = client_router.clone();

    let client = MpscClientTransport::new(client_tx, server_rx, client_router);
    let server = MpscServerTransport::new(client_rx, server_tx, server_router);

    (client, server)
}

#[cfg(test)]
#[path = "mpsc_test.rs"]
mod tests;
