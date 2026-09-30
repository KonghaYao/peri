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
    Blocking(mpsc::Sender<io::Result<OAuthCredentialValue>>),
}

impl Reply {
    fn send(self, value: io::Result<OAuthCredentialValue>) {
        match self {
            Self::Async(reply) => {
                let _ = reply.send(value);
            }
            Self::Blocking(reply) => {
                let _ = reply.send(value);
            }
        }
    }
}

struct Job {
    request: OAuthCredentialRequest,
    deadline: tokio::time::Instant,
    reply: Reply,
}

#[derive(Clone)]
pub struct OAuthCredentialClient {
    sender: tokio::sync::mpsc::UnboundedSender<Job>,
}

impl OAuthCredentialClient {
    pub fn new(store: Arc<dyn OAuthCredentialPort>) -> io::Result<Self> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let (ready, startup) = mpsc::channel();
        std::thread::Builder::new()
            .name("peri-credentials-mcp".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready.send(Err(unavailable()));
                        return;
                    }
                };
                tracing::subscriber::with_default(
                    tracing::subscriber::NoSubscriber::default(),
                    || {
                        runtime.block_on(async move {
                            let (client_io, server_io) = tokio::io::duplex(65536);
                            let mut server = tokio::spawn(async move {
                                if let Ok(service) =
                                    OAuthCredentialMcpServer::new(store).serve(server_io).await
                                {
                                    let _ = service.waiting().await;
                                }
                            });
                            let mut client =
                                match tokio::time::timeout(REQUEST_TIMEOUT, ().serve(client_io))
                                    .await
                                {
                                    Ok(Ok(client)) => client,
                                    _ => {
                                        let _ = ready.send(Err(unavailable()));
                                        server.abort();
                                        return;
                                    }
                                };
                            if ready.send(Ok(())).is_err() {
                                let _ = client.close_with_timeout(Duration::from_secs(1)).await;
                                server.abort();
                                return;
                            }
                            while let Some(job) = receiver.recv().await {
                                let result = async {
                                    let params = serde_json::to_value(job.request)
                                        .map_err(|_| invalid_data())?;
                                    let handle = tokio::time::timeout_at(
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
                                    .map_err(|_| timed_out())?
                                    .map_err(|_| unavailable())?;
                                    let request_id = handle.id.clone();
                                    let response = match tokio::time::timeout_at(
                                        job.deadline,
                                        handle.await_response(),
                                    )
                                    .await
                                    {
                                        Ok(response) => response.map_err(|_| unavailable())?,
                                        Err(_) => {
                                            let _ = tokio::time::timeout(
                                                Duration::from_secs(1),
                                                client.peer().notify_cancelled(
                                                    rmcp::model::CancelledNotificationParam::new(
                                                        Some(request_id),
                                                        Some("credential request timed out".into()),
                                                    ),
                                                ),
                                            )
                                            .await;
                                            return Err(timed_out());
                                        }
                                    };
                                    let ServerResult::CustomResult(response) = response else {
                                        return Err(invalid_data());
                                    };
                                    let response: OAuthCredentialResponse =
                                        serde_json::from_value(response.0)
                                            .map_err(|_| invalid_data())?;
                                    response.map_err(storage_error)
                                }
                                .await;
                                job.reply.send(result);
                            }
                            let _ = client.close_with_timeout(Duration::from_secs(1)).await;
                            if tokio::time::timeout(Duration::from_secs(1), &mut server)
                                .await
                                .is_err()
                            {
                                server.abort();
                                let _ = server.await;
                            }
                        })
                    },
                );
            })?;
        startup
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|_| timed_out())??;
        Ok(Self { sender })
    }

    async fn request(&self, request: OAuthCredentialRequest) -> io::Result<OAuthCredentialValue> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        self.sender
            .send(Job {
                request,
                deadline,
                reply: Reply::Async(reply),
            })
            .map_err(|_| unavailable())?;
        tokio::time::timeout_at(deadline, response)
            .await
            .map_err(|_| timed_out())?
            .map_err(|_| unavailable())?
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
                    .map_err(|_| invalid_data())
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
        let credentials = serde_json::to_string(&credentials).map_err(|_| invalid_data())?;
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

    pub fn clear_server_blocking(&self, server_key: &str) -> io::Result<()> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(Job {
                request: OAuthCredentialRequest::Clear {
                    server_key: server_key.to_owned(),
                },
                deadline: tokio::time::Instant::now() + REQUEST_TIMEOUT,
                reply: Reply::Blocking(reply),
            })
            .map_err(|_| unavailable())?;
        match response
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|_| timed_out())??
        {
            OAuthCredentialValue::Saved => Ok(()),
            _ => Err(invalid_data()),
        }
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

fn unavailable() -> io::Error {
    io::Error::other("OAuth credential MCP is unavailable")
}

fn invalid_data() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "OAuth credential response is invalid",
    )
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "OAuth credential operation timed out; outcome is not confirmed",
    )
}
