use std::{
    io,
    sync::{mpsc, Arc},
    time::Duration,
};

use peri_acp_types::oauth_credentials::{
    OAuthCredentialPort, OAuthCredentialRequest, OAuthCredentialResponse, OAuthCredentialValue,
    OAUTH_CREDENTIAL_METHOD,
};
use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    service::PeerRequestOptions,
    transport::auth::StoredCredentials,
    ServiceExt,
};

use crate::{server::storage_error, OAuthCredentialMcpServer};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

enum Reply {
    Async(tokio::sync::oneshot::Sender<io::Result<OAuthCredentialValue>>),
    #[cfg(not(target_os = "emscripten"))]
    Blocking(mpsc::Sender<io::Result<OAuthCredentialValue>>),
}

impl Reply {
    fn send(self, value: io::Result<OAuthCredentialValue>) {
        match self {
            Self::Async(reply) => {
                let _ = reply.send(value);
            }
            #[cfg(not(target_os = "emscripten"))]
            Self::Blocking(reply) => {
                let _ = reply.send(value);
            }
        }
    }
}

struct Job {
    request: OAuthCredentialRequest,
    deadline: std::time::Instant,
    reply: Reply,
}

#[derive(Clone)]
pub struct OAuthCredentialClient {
    sender: tokio::sync::mpsc::UnboundedSender<Job>,
}

impl OAuthCredentialClient {
    #[cfg(not(target_os = "emscripten"))]
    pub fn new(store: Arc<dyn OAuthCredentialPort>) -> io::Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let (ready, startup) = mpsc::channel();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        std::thread::Builder::new()
            .name("peri-credentials-mcp".into())
            .spawn(move || {
                let _dispatcher = tracing::dispatcher::set_default(&dispatcher);
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(diagnostic_error(
                            io::ErrorKind::Other,
                            "OAuth credential runtime initialization failed",
                            error,
                        )));
                        return;
                    }
                };
                runtime.block_on(run_worker(store, receiver, Some(ready)));
            })?;
        startup
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::TimedOut,
                    "OAuth credential startup failed",
                    error,
                )
            })??;
        Ok(Self { sender })
    }

    #[cfg(target_os = "emscripten")]
    pub fn new(store: Arc<dyn OAuthCredentialPort>) -> io::Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<Job>();
        spawn_worker(store, receiver)?;
        Ok(Self { sender })
    }

    async fn request(&self, request: OAuthCredentialRequest) -> io::Result<OAuthCredentialValue> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let deadline = peri_time::monotonic_now() + REQUEST_TIMEOUT;
        self.sender
            .send(Job {
                request,
                deadline,
                reply: Reply::Async(reply),
            })
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::Other,
                    "OAuth credential job submission failed",
                    error,
                )
            })?;
        peri_time::timeout_at(deadline, response)
            .await
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::TimedOut,
                    "OAuth credential operation timed out; outcome is not confirmed",
                    error,
                )
            })?
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::Other,
                    "OAuth credential worker reply failed",
                    error,
                )
            })?
    }

    pub async fn load_server(&self, server_key: &str) -> io::Result<Option<StoredCredentials>> {
        match self
            .request(OAuthCredentialRequest::Load {
                server_key: server_key.to_owned(),
            })
            .await?
        {
            OAuthCredentialValue::Credentials(Some(credentials)) => {
                serde_json::from_str(&credentials)
                    .map(Some)
                    .map_err(|error| {
                        diagnostic_error(
                            io::ErrorKind::InvalidData,
                            "OAuth credential decoding failed",
                            error,
                        )
                    })
            }
            OAuthCredentialValue::Credentials(None) => Ok(None),
            _ => Err(invalid_data()),
        }
    }

    pub async fn save_server(
        &self,
        server_key: &str,
        credentials: StoredCredentials,
    ) -> io::Result<()> {
        let credentials = serde_json::to_string(&credentials).map_err(|error| {
            diagnostic_error(
                io::ErrorKind::InvalidData,
                "OAuth credential encoding failed",
                error,
            )
        })?;
        match self
            .request(OAuthCredentialRequest::Save {
                server_key: server_key.to_owned(),
                credentials,
            })
            .await?
        {
            OAuthCredentialValue::Saved => Ok(()),
            _ => Err(invalid_data()),
        }
    }

    pub async fn clear_server(&self, server_key: &str) -> io::Result<()> {
        match self
            .request(OAuthCredentialRequest::Clear {
                server_key: server_key.to_owned(),
            })
            .await?
        {
            OAuthCredentialValue::Saved => Ok(()),
            _ => Err(invalid_data()),
        }
    }

    #[cfg(not(target_os = "emscripten"))]
    pub fn clear_server_blocking(&self, server_key: &str) -> io::Result<()> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(Job {
                request: OAuthCredentialRequest::Clear {
                    server_key: server_key.to_owned(),
                },
                deadline: peri_time::monotonic_now() + REQUEST_TIMEOUT,
                reply: Reply::Blocking(reply),
            })
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::Other,
                    "OAuth credential job submission failed",
                    error,
                )
            })?;
        match response
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::TimedOut,
                    "OAuth credential operation timed out; outcome is not confirmed",
                    error,
                )
            })?? {
            OAuthCredentialValue::Saved => Ok(()),
            _ => Err(invalid_data()),
        }
    }

    #[cfg(target_os = "emscripten")]
    pub fn clear_server_blocking(&self, server_key: &str) -> io::Result<()> {
        let client = self.clone();
        let key = server_key.to_owned();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = client.clear_server(&key).await {
                tracing::error!(%error, "dynamic MCP OAuth credential rollback failed during drop");
            }
        });
        Ok(())
    }

    pub async fn clear_all(&self) -> io::Result<()> {
        match self.request(OAuthCredentialRequest::ClearAll).await? {
            OAuthCredentialValue::Saved => Ok(()),
            _ => Err(invalid_data()),
        }
    }

    pub async fn list_servers(&self) -> io::Result<Vec<String>> {
        match self.request(OAuthCredentialRequest::List).await? {
            OAuthCredentialValue::Keys(keys) => Ok(keys),
            _ => Err(invalid_data()),
        }
    }
}

#[cfg(any(target_os = "emscripten", test))]
fn spawn_worker(
    store: Arc<dyn OAuthCredentialPort>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<Job>,
) -> io::Result<tokio::task::JoinHandle<()>> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
        diagnostic_error(
            io::ErrorKind::Other,
            "OAuth credential runtime is unavailable",
            error,
        )
    })?;
    Ok(runtime.spawn(run_worker(store, receiver, None)))
}

async fn run_worker(
    store: Arc<dyn OAuthCredentialPort>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<Job>,
    ready: Option<mpsc::Sender<io::Result<()>>>,
) {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server_task = async move {
        match OAuthCredentialMcpServer::new(store).serve(server_io).await {
            Ok(service) => {
                if let Err(error) = service.waiting().await {
                    tracing::warn!(%error, "OAuth credential server failed");
                }
            }
            Err(error) => tracing::warn!(%error, "OAuth credential server initialization failed"),
        }
    };
    let mut server = tokio::spawn(server_task);
    let mut client = match peri_time::timeout(REQUEST_TIMEOUT, ().serve(client_io)).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            if let Some(ready) = ready {
                let _ = ready.send(Err(diagnostic_error(
                    io::ErrorKind::Other,
                    "OAuth credential MCP initialization failed",
                    error,
                )));
            } else {
                tracing::warn!(%error, "OAuth credential MCP initialization failed");
            }
            server.abort();
            let _ = server.await;
            return;
        }
        Err(error) => {
            let error = diagnostic_error(
                io::ErrorKind::TimedOut,
                "OAuth credential MCP initialization timed out",
                error,
            );
            if let Some(ready) = ready {
                let _ = ready.send(Err(error));
            }
            server.abort();
            let _ = server.await;
            return;
        }
    };
    if ready.is_some_and(|ready| ready.send(Ok(())).is_err()) {
        let _ = client.close_with_timeout(Duration::from_secs(1)).await;
        server.abort();
        let _ = server.await;
        return;
    }
    while let Some(job) = receiver.recv().await {
        let result = async {
            let params = serde_json::to_value(job.request).map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::InvalidData,
                    "OAuth credential request encoding failed",
                    error,
                )
            })?;
            let handle = peri_time::timeout_at(
                job.deadline,
                client.peer().send_request_with_option(
                    ClientRequest::CustomRequest(CustomRequest::new(
                        OAUTH_CREDENTIAL_METHOD,
                        Some(params),
                    )),
                    PeerRequestOptions::no_options(),
                ),
            )
            .await
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::TimedOut,
                    "OAuth credential operation timed out; outcome is not confirmed",
                    error,
                )
            })?
            .map_err(|error| {
                diagnostic_error(
                    io::ErrorKind::Other,
                    "OAuth credential request failed",
                    error,
                )
            })?;
            let request_id = handle.id.clone();
            let response = match peri_time::timeout_at(job.deadline, handle.await_response()).await
            {
                Ok(response) => response.map_err(|error| {
                    diagnostic_error(
                        io::ErrorKind::Other,
                        "OAuth credential response failed",
                        error,
                    )
                })?,
                Err(error) => {
                    let _ = peri_time::timeout(
                        Duration::from_secs(1),
                        client.peer().notify_cancelled(
                            rmcp::model::CancelledNotificationParam::new(
                                Some(request_id),
                                Some("credential request timed out".into()),
                            ),
                        ),
                    )
                    .await;
                    return Err(diagnostic_error(
                        io::ErrorKind::TimedOut,
                        "OAuth credential operation timed out; outcome is not confirmed",
                        error,
                    ));
                }
            };
            let ServerResult::CustomResult(response) = response else {
                return Err(invalid_data());
            };
            let response: OAuthCredentialResponse =
                serde_json::from_value(response.0).map_err(|error| {
                    diagnostic_error(
                        io::ErrorKind::InvalidData,
                        "OAuth credential response decoding failed",
                        error,
                    )
                })?;
            response.map_err(storage_error)
        }
        .await;
        job.reply.send(result);
    }
    let _ = client.close_with_timeout(Duration::from_secs(1)).await;
    if peri_time::timeout(Duration::from_secs(1), &mut server)
        .await
        .is_err()
    {
        server.abort();
        let _ = server.await;
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{operation}: {source}")]
struct CredentialClientFailure {
    operation: &'static str,
    source: Box<dyn std::error::Error + Send + Sync>,
}

fn diagnostic_error(
    kind: io::ErrorKind,
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> io::Error {
    let error = CredentialClientFailure {
        operation,
        source: Box::new(source),
    };
    tracing::warn!(%error, "OAuth credential client failed");
    io::Error::new(kind, error)
}

fn invalid_data() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "OAuth credential response is invalid",
    )
}
#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
