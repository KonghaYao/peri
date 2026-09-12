//! rmcp service 适配与连接能力声明。

use crate::mcp::channel_handler::ChannelHandler;
use rmcp::{
    model::{ClientCapabilities, Implementation, InitializeRequestParams},
    service::{Peer, QuitReason, RoleClient, RunningService},
};
use std::sync::Arc;

/// Shared with the session pool until the original protocol worker has joined.
pub(crate) struct McpServiceOwner {
    service: tokio::sync::Mutex<McpServiceWrapper>,
    cancellation: parking_lot::Mutex<Option<rmcp::service::RunningServiceCancellationToken>>,
    stopped: std::sync::atomic::AtomicBool,
}

impl McpServiceOwner {
    pub(crate) fn new(service: McpServiceWrapper) -> Self {
        let cancellation = match &service {
            McpServiceWrapper::Default(service) => Some(service.cancellation_token()),
            McpServiceWrapper::Channel(service) => Some(service.cancellation_token()),
            _ => None,
        };
        Self {
            service: tokio::sync::Mutex::new(service),
            cancellation: parking_lot::Mutex::new(cancellation),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn begin_close(&self) {
        if let Some(token) = self.cancellation.lock().take() {
            token.cancel();
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) async fn close_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        self.begin_close();
        let mut service = self.service.lock().await;
        if self.is_stopped() {
            return Ok(Some(QuitReason::Closed));
        }
        let result = service.close_with_timeout(timeout).await;
        if !matches!(result, Ok(None)) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }
}

impl Drop for McpServiceOwner {
    fn drop(&mut self) {
        self.begin_close();
    }
}

/// Wrapper for RunningService that can hold either handler type
pub(crate) enum McpServiceWrapper {
    Default(RunningService<RoleClient, InitializeRequestParams>),
    Channel(RunningService<RoleClient, Arc<ChannelHandler>>),
    Closing(tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>),
    Closed,
    Shared(McpServiceHandle),
    #[cfg(test)]
    Controlled(ControlledMcpService),
}

pub(crate) struct McpServiceHandle {
    owner: Arc<McpServiceOwner>,
    peer: Peer<RoleClient>,
}

impl Drop for McpServiceHandle {
    fn drop(&mut self) {
        self.owner.begin_close();
    }
}

#[cfg(test)]
pub(crate) struct ControlledMcpService {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
    close_count: Arc<std::sync::atomic::AtomicUsize>,
    completes: bool,
}

#[cfg(test)]
impl ControlledMcpService {
    pub(crate) fn new(
        entered: tokio::sync::oneshot::Sender<()>,
        release: Arc<tokio::sync::Notify>,
        close_count: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            entered: std::sync::Mutex::new(Some(entered)),
            release,
            close_count,
            completes: true,
        }
    }

    pub(crate) fn timing_out(close_count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        let (entered, _entered_rx) = tokio::sync::oneshot::channel();
        Self {
            entered: std::sync::Mutex::new(Some(entered)),
            release: Arc::new(tokio::sync::Notify::new()),
            close_count,
            completes: false,
        }
    }

    async fn close(&mut self) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        self.close_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(entered) = self
            .entered
            .lock()
            .expect("controlled service poisoned")
            .take()
        {
            let _ = entered.send(());
        }
        if self.completes {
            self.release.notified().await;
            Ok(Some(QuitReason::Closed))
        } else {
            Ok(None)
        }
    }
}

impl McpServiceWrapper {
    pub(crate) fn shared(owner: Arc<McpServiceOwner>, peer: Peer<RoleClient>) -> Self {
        Self::Shared(McpServiceHandle { owner, peer })
    }

    pub async fn close_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        // rmcp's timed close consumes its internal join handle before awaiting it.
        // Keep our own worker handle across timeout/cancellation so retry still waits
        // for the original protocol service and transport cleanup.
        if matches!(self, Self::Default(_) | Self::Channel(_)) {
            let service = std::mem::replace(self, Self::Closed);
            *self = Self::Closing(tokio::spawn(async move {
                match service {
                    Self::Default(service) => service.cancel().await,
                    Self::Channel(service) => service.cancel().await,
                    _ => unreachable!(),
                }
            }));
        }
        match self {
            Self::Closing(handle) => match tokio::time::timeout(timeout, handle).await {
                Ok(result) => {
                    *self = Self::Closed;
                    match result {
                        Ok(result) => result.map(Some),
                        Err(error) => Err(error),
                    }
                }
                Err(_) => Ok(None),
            },
            Self::Closed => Ok(Some(QuitReason::Closed)),
            Self::Shared(service) => Box::pin(service.owner.close_with_timeout(timeout)).await,
            Self::Default(_) | Self::Channel(_) => unreachable!(),
            #[cfg(test)]
            McpServiceWrapper::Controlled(svc) => svc.close().await,
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        match self {
            McpServiceWrapper::Default(svc) => svc.peer(),
            McpServiceWrapper::Channel(svc) => svc.peer(),
            McpServiceWrapper::Shared(service) => &service.peer,
            McpServiceWrapper::Closing(_) | McpServiceWrapper::Closed => {
                panic!("closing service has no active protocol peer")
            }
            #[cfg(test)]
            McpServiceWrapper::Controlled(_) => {
                panic!("controlled test service has no protocol peer")
            }
        }
    }
}

pub(crate) const SERVER_CACHE_VERSION_EXTENSION: &str = "io.mcpp/server-cache-version";

pub(crate) fn mcpp_client_info_for_profile(
    profile: &crate::mcp::apps::McpCapabilityProfile,
) -> InitializeRequestParams {
    let mut extensions = std::collections::BTreeMap::from([(
        SERVER_CACHE_VERSION_EXTENSION.to_string(),
        serde_json::Map::new(),
    )]);
    if let Some(extension) = profile.ui_extension() {
        extensions.insert(crate::mcp::apps::MCP_UI_EXTENSION.to_string(), extension);
    }
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some(extensions);
    InitializeRequestParams::new(capabilities, Implementation::from_build_env())
}

pub(crate) fn peer_cache_version(peer: &Peer<RoleClient>) -> Option<String> {
    peer.peer_info()?
        .capabilities
        .extensions
        .as_ref()?
        .get(SERVER_CACHE_VERSION_EXTENSION)?
        .get("cacheVersion")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// SEP-2640 Skills 扩展标识（capabilities.extensions 键）。
pub(crate) const SKILLS_EXTENSION_ID: &str = "io.modelcontextprotocol/skills";

/// 检测 peer 的 server capabilities 是否声明 Skills 扩展（SEP-2640）。
///
/// 仅凭 scheme 不得判定资源为技能（规范 MUST NOT），扩展声明是规范路径的
/// 唯一门闩；未声明时调用 `skills/list` 属于对不支持方法的盲调。
pub(crate) fn peer_declares_skills(peer: &Peer<RoleClient>) -> bool {
    peer.peer_info()
        .map(|info| {
            info.capabilities
                .extensions
                .as_ref()
                .map(|ext| ext.contains_key(SKILLS_EXTENSION_ID))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
