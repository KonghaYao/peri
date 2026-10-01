use std::{
    io,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use peri_acp_types::configuration::{
    ConfigurationErrorKind, ConfigurationPaths, ConfigurationRequest, ConfigurationResponse,
    ConfigurationValue, CONFIGURATION_METHOD,
};
use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    service::PeerRequestOptions,
    ServiceExt,
};

use crate::ConfigurationMcpServer;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Job {
    request: ConfigurationRequest,
    deadline: tokio::time::Instant,
    reply: mpsc::Sender<io::Result<ConfigurationValue>>,
}

#[derive(Clone)]
pub struct ConfigurationClient {
    sender: tokio::sync::mpsc::UnboundedSender<Job>,
}

enum Connection {
    Local,
    Tcp(String),
}

impl ConfigurationClient {
    pub fn local() -> io::Result<Self> {
        Self::connect(Connection::Local)
    }

    pub fn connect_tcp(address: impl Into<String>) -> io::Result<Self> {
        Self::connect(Connection::Tcp(address.into()))
    }

    fn connect(connection: Connection) -> io::Result<Self> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let (ready, startup) = mpsc::channel();
        std::thread::Builder::new()
            .name("peri-config-mcp".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let client = match connection {
                        Connection::Local => {
                            let (client_io, server_io) = tokio::io::duplex(65536);
                            tokio::spawn(async move {
                                if let Ok(server) =
                                    ConfigurationMcpServer::new().serve(server_io).await
                                {
                                    let _ = server.waiting().await;
                                }
                            });
                            match tokio::time::timeout(REQUEST_TIMEOUT, ().serve(client_io)).await {
                                Ok(result) => {
                                    result.map_err(|error| io::Error::other(error.to_string()))
                                }
                                Err(_) => Err(timeout_error()),
                            }
                        }
                        Connection::Tcp(address) => {
                            match tokio::time::timeout(
                                REQUEST_TIMEOUT,
                                tokio::net::TcpStream::connect(address),
                            )
                            .await
                            {
                                Ok(Ok(transport)) => {
                                    match tokio::time::timeout(REQUEST_TIMEOUT, ().serve(transport))
                                        .await
                                    {
                                        Ok(result) => result
                                            .map_err(|error| io::Error::other(error.to_string())),
                                        Err(_) => Err(timeout_error()),
                                    }
                                }
                                Ok(Err(error)) => Err(error),
                                Err(_) => Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "configuration connection timed out",
                                )),
                            }
                        }
                    };
                    let mut client = match client {
                        Ok(client) => client,
                        Err(error) => {
                            let _ = ready.send(Err(error));
                            return;
                        }
                    };
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    while let Some(job) = receiver.recv().await {
                        let response = async {
                            if job.deadline <= tokio::time::Instant::now() {
                                return Err(timeout_error());
                            }
                            let params = serde_json::to_value(job.request).map_err(|error| {
                                io::Error::new(io::ErrorKind::InvalidInput, error)
                            })?;
                            let request = tokio::time::timeout_at(
                                job.deadline,
                                client.peer().send_request_with_option(
                                    ClientRequest::CustomRequest(CustomRequest::new(
                                        CONFIGURATION_METHOD,
                                        Some(params),
                                    )),
                                    PeerRequestOptions::no_options(),
                                ),
                            )
                            .await
                            .map_err(|_| timeout_error())?
                            .map_err(|error| io::Error::other(error.to_string()))?;
                            let id = request.id.clone();
                            let response = match tokio::time::timeout_at(
                                job.deadline,
                                request.await_response(),
                            )
                            .await
                            {
                                Ok(result) => {
                                    result.map_err(|error| io::Error::other(error.to_string()))?
                                }
                                Err(_) => {
                                    let _ = tokio::time::timeout(
                                        Duration::from_secs(1),
                                        client.peer().notify_cancelled(
                                            rmcp::model::CancelledNotificationParam::new(
                                                Some(id),
                                                Some("configuration request timed out".into()),
                                            ),
                                        ),
                                    )
                                    .await;
                                    return Err(timeout_error());
                                }
                            };
                            let ServerResult::CustomResult(response) = response else {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "invalid configuration response",
                                ));
                            };
                            let response: ConfigurationResponse =
                                serde_json::from_value(response.0).map_err(|error| {
                                    io::Error::new(io::ErrorKind::InvalidData, error)
                                })?;
                            response.map_err(|failure| {
                                let kind = match failure.kind {
                                    ConfigurationErrorKind::NotFound => io::ErrorKind::NotFound,
                                    ConfigurationErrorKind::PermissionDenied => {
                                        io::ErrorKind::PermissionDenied
                                    }
                                    ConfigurationErrorKind::AlreadyExists => {
                                        io::ErrorKind::AlreadyExists
                                    }
                                    ConfigurationErrorKind::InvalidInput => {
                                        io::ErrorKind::InvalidInput
                                    }
                                    ConfigurationErrorKind::InvalidData => {
                                        io::ErrorKind::InvalidData
                                    }
                                    ConfigurationErrorKind::TimedOut => io::ErrorKind::TimedOut,
                                    ConfigurationErrorKind::Other => io::ErrorKind::Other,
                                };
                                io::Error::new(kind, failure.message)
                            })
                        }
                        .await;
                        let _ = job.reply.send(response);
                    }
                    let _ = client.close_with_timeout(Duration::from_secs(1)).await;
                });
            })?;
        startup
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|_| timeout_error())??;
        Ok(Self { sender })
    }

    fn request(&self, request: ConfigurationRequest) -> io::Result<ConfigurationValue> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(Job {
                request,
                deadline: tokio::time::Instant::now() + REQUEST_TIMEOUT,
                reply,
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "configuration data plane unavailable",
                )
            })?;
        response
            .recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2))
            .map_err(|_| timeout_error())?
    }

    pub fn read_text(&self, path: &Path) -> io::Result<String> {
        match self.request(ConfigurationRequest::ReadText { path: path.into() })? {
            ConfigurationValue::Text(text) => Ok(text),
            _ => Err(invalid_response()),
        }
    }

    pub fn read_environment(&self, name: &str) -> io::Result<Option<String>> {
        match self.request(ConfigurationRequest::ReadEnvironment { name: name.into() })? {
            ConfigurationValue::Environment(value) => Ok(value),
            _ => Err(invalid_response()),
        }
    }

    pub fn write_text_atomic(&self, path: &Path, content: &str) -> io::Result<()> {
        match self.request(ConfigurationRequest::WriteTextAtomic {
            path: path.into(),
            content: content.into(),
        })? {
            ConfigurationValue::Written => Ok(()),
            _ => Err(invalid_response()),
        }
    }

    pub fn write_text_if_unchanged(
        &self,
        path: &Path,
        expected: &Option<String>,
        content: &str,
    ) -> io::Result<bool> {
        match self.request(ConfigurationRequest::WriteTextIfUnchanged {
            path: path.into(),
            expected: expected.clone(),
            content: content.into(),
        })? {
            ConfigurationValue::Bool(written) => Ok(written),
            _ => Err(invalid_response()),
        }
    }

    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        match self.request(ConfigurationRequest::Exists { path: path.into() })? {
            ConfigurationValue::Bool(exists) => Ok(exists),
            _ => Err(invalid_response()),
        }
    }

    pub fn same_file(&self, first: &Path, second: &Path) -> io::Result<bool> {
        match self.request(ConfigurationRequest::SameFile {
            first: first.into(),
            second: second.into(),
        })? {
            ConfigurationValue::Bool(same) => Ok(same),
            _ => Err(invalid_response()),
        }
    }

    pub fn paths(&self) -> io::Result<ConfigurationPaths> {
        match self.request(ConfigurationRequest::Paths)? {
            ConfigurationValue::Paths(paths) => Ok(paths),
            _ => Err(invalid_response()),
        }
    }

    pub fn set_global_config_path(&self, path: Option<PathBuf>) -> io::Result<()> {
        match self.request(ConfigurationRequest::SetGlobalPath { path })? {
            ConfigurationValue::Written => Ok(()),
            _ => Err(invalid_response()),
        }
    }
}

fn timeout_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "configuration request timed out")
}

fn invalid_response() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid configuration response")
}
