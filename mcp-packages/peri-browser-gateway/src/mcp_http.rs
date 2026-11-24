//! 保留 rmcp 的 HTTP、SSE 和重放协议，仅在有状态传输内附加可信会话许可。

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use rmcp::model::{ClientJsonRpcMessage, GetExtensions, ServerJsonRpcMessage, ServerResult};
use rmcp::service::RoleServer;
use rmcp::transport::streamable_http_server::session::local::{
    LocalSessionManager, SessionTransport,
};
use rmcp::transport::streamable_http_server::session::{
    ServerSseMessage, SessionId, SessionManager,
};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::transport::Transport;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::server::BrowserMcpServer;
use crate::session::{
    CloseReason, SessionDirectory, SessionEntry, SettlementError, SETTLE_TIMEOUT,
};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct Initializing;

#[derive(Clone)]
pub(crate) struct BoundBrowserSession(pub Arc<SessionEntry>);

pub(crate) struct GatewaySessions {
    sdk: Arc<LocalSessionManager>,
    directory: Arc<SessionDirectory>,
}

pub(crate) fn service(
    directory: Arc<SessionDirectory>,
    stop: CancellationToken,
    keep_alive: Duration,
) -> StreamableHttpService<BrowserMcpServer, GatewaySessions> {
    let mut sdk = LocalSessionManager::default();
    sdk.session_config.keep_alive = Some(keep_alive);
    StreamableHttpService::new(
        || Ok(BrowserMcpServer),
        Arc::new(GatewaySessions {
            sdk: Arc::new(sdk),
            directory,
        }),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_cancellation_token(stop)
            .enforce_origin_validation(),
    )
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("session is not active")]
    Inactive,
    #[error("session initialization failed")]
    Initialize,
    #[error("session transport request failed")]
    Transport,
    #[error(transparent)]
    Settlement(#[from] SettlementError),
}

impl GatewaySessions {
    fn live(&self, id: &SessionId) -> Result<Arc<SessionEntry>, SessionError> {
        self.directory
            .entry(id)
            .filter(|entry| entry.live())
            .ok_or(SessionError::Inactive)
    }
}

// HTTP initialize future 被丢弃时也必须收尾；不能依赖请求 future 执行清理。
struct InitializeGuard(Option<Arc<SessionEntry>>);

impl Drop for InitializeGuard {
    fn drop(&mut self) {
        if let Some(entry) = self.0.take() {
            entry.begin_close(CloseReason::Initialize);
        }
    }
}

impl SessionManager for GatewaySessions {
    type Error = SessionError;
    type Transport = BoundTransport;

    async fn create_session(&self) -> Result<(SessionId, BoundTransport), SessionError> {
        let permit = self
            .directory
            .create_permit()
            .ok_or(SessionError::Inactive)?;
        let (id, inner) = self
            .sdk
            .create_session()
            .await
            .map_err(|_| SessionError::Transport)?;
        let entry = self.directory.initialize(id.clone());
        let (release, released) = oneshot::channel();
        let dir = self.directory.clone();
        let sdk = self.sdk.clone();
        let (key, record) = (id.clone(), entry.clone());
        dir.finalizers.clone().spawn(async move {
            let _permit = permit;
            // 即使 HTTP 层在 create 与 initialize 间中止，也有独立上限。
            tokio::select! {
                _ = record.stop.cancelled() => {},
                _ = tokio::time::sleep(INITIALIZE_TIMEOUT) => {
                    record.expire_initialize();
                    record.stop.cancelled().await;
                }
            }
            let transport = async {
                let mut inner: SessionTransport =
                    released.await.map_err(|_| SettlementError::Transport)?;
                inner.cancel_token().cancel();
                while inner.receive().await.is_some() {} // 解除 SDK worker 的入队阻塞，不 dispatch。
                let joined = inner.close().await;
                let removed = sdk.close_session(&key).await;
                joined.map_err(|_| SettlementError::Transport)?;
                removed.map_err(|_| SettlementError::Transport)
            };
            let mut cleanup = std::pin::pin!(async {
                let (transport, domain) = tokio::join!(transport, record.settle_domain());
                transport.and(domain)
            });
            let outcome = match tokio::time::timeout(SETTLE_TIMEOUT, &mut cleanup).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    record.report_deadline();
                    // 对调用者报错，但仍持有 join 和 Closing 记录，不把 deadline 当成结算。
                    let _ = cleanup.await;
                    Err(SettlementError::Deadline)
                }
            };
            dir.finish(&key, &record, outcome);
        });
        Ok((
            id,
            BoundTransport {
                inner: Some(inner),
                release: Some(release),
                entry,
            },
        ))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        mut message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, SessionError> {
        let entry = self.directory.entry(id).ok_or(SessionError::Inactive)?;
        if !entry.start_initialize() {
            return Err(SessionError::Inactive);
        }
        let mut guard = InitializeGuard(Some(entry.clone()));
        if let ClientJsonRpcMessage::Request(request) = &mut message {
            request.request.extensions_mut().insert(Initializing);
        }
        let response = tokio::select! {
            biased;
            _ = entry.stop.cancelled() => return Err(SessionError::Initialize),
            response = self.sdk.initialize_session(id, message) => response.map_err(|_| SessionError::Initialize)?,
        };
        if !matches!(&response, ServerJsonRpcMessage::Response(r) if matches!(r.result, ServerResult::InitializeResult(_)))
            || !self.directory.publish(&entry)
        {
            return Err(SessionError::Initialize);
        }
        guard.0 = None;
        Ok(response)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, SessionError> {
        let Ok(entry) = self.live(id) else {
            return Ok(false);
        };
        let exists = self
            .sdk
            .has_session(id)
            .await
            .map_err(|_| SessionError::Transport)?;
        Ok(exists && entry.live())
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), SessionError> {
        if let Some(entry) = self.directory.entry(id) {
            entry.begin_close(CloseReason::Delete);
            entry.wait().await?;
        }
        Ok(())
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        mut message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, SessionError> {
        let entry = self.live(id)?;
        if let ClientJsonRpcMessage::Request(request) = &mut message {
            request
                .request
                .extensions_mut()
                .insert(BoundBrowserSession(entry.clone()));
        }
        relay(&entry, self.sdk.create_stream(id, message)).await
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), SessionError> {
        let entry = self.live(id)?;
        let (_core, _permit) = entry.admit().ok_or(SessionError::Inactive)?;
        tokio::select! {
            biased;
            _ = entry.stop.cancelled() => Err(SessionError::Inactive),
            result = self.sdk.accept_message(id, message) => result.map_err(|_| SessionError::Transport),
        }
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, SessionError> {
        let entry = self.live(id)?;
        relay(&entry, self.sdk.create_standalone_stream(id)).await
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, SessionError> {
        let entry = self.live(id)?;
        relay(&entry, self.sdk.resume(id, last_event_id)).await
    }
}

async fn relay<S, E>(
    entry: &SessionEntry,
    create: impl Future<Output = Result<S, E>>,
) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, SessionError>
where
    S: Stream<Item = ServerSseMessage> + Send + 'static,
{
    let (_core, permit) = entry.admit().ok_or(SessionError::Inactive)?;
    let stop = entry.stop.clone();
    let stream = tokio::select! {
        biased;
        _ = stop.cancelled() => return Err(SessionError::Inactive),
        stream = create => stream.map_err(|_| SessionError::Transport)?,
    };
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let _permit = permit;
        tokio::pin!(stream);
        loop {
            let item = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = tx.closed() => break,
                item = stream.next() => item,
            };
            let Some(item) = item else { break };
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                sent = tx.send(item) => if sent.is_err() { break },
            }
        }
    });
    Ok(futures::stream::unfold(
        (rx, entry.stop.clone()),
        |(mut rx, stop)| async move {
            tokio::select! {
                biased;
                _ = stop.cancelled() => None,
                item = rx.recv() => item.map(|item| (item, (rx, stop))),
            }
        },
    ))
}

pub(crate) struct BoundTransport {
    inner: Option<SessionTransport>,
    release: Option<oneshot::Sender<SessionTransport>>,
    entry: Arc<SessionEntry>,
}

impl BoundTransport {
    fn release(&mut self) {
        self.entry.begin_close(CloseReason::Transport);
        if let (Some(tx), Some(inner)) = (self.release.take(), self.inner.take()) {
            let _ = tx.send(inner);
        }
    }
}

impl Transport<RoleServer> for BoundTransport {
    type Error = SessionError;

    fn send(
        &mut self,
        message: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send + 'static {
        let stop = self.entry.stop.clone();
        let send = self.inner.as_mut().map(|inner| inner.send(message));
        async move {
            let send = send.ok_or(SessionError::Inactive)?;
            tokio::select! {
                biased;
                _ = stop.cancelled() => Err(SessionError::Inactive),
                result = send => result.map_err(|_| SessionError::Transport),
            }
        }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        let message = tokio::select! {
            biased;
            _ = self.entry.stop.cancelled() => None,
            message = self.inner.as_mut()?.receive() => message,
        };
        if message.is_none() {
            self.release();
        }
        message
    }

    async fn close(&mut self) -> Result<(), SessionError> {
        self.release();
        Ok(())
    }
}

impl Drop for BoundTransport {
    fn drop(&mut self) {
        self.release();
    }
}
