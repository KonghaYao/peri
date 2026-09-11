//! rmcp service 适配与连接能力声明。

use crate::mcp::channel_handler::ChannelHandler;
use rmcp::{
    model::{ClientCapabilities, Implementation, InitializeRequestParams},
    service::{Peer, QuitReason, RoleClient, RunningService},
};
use std::sync::Arc;

/// Wrapper for RunningService that can hold either handler type
pub(crate) enum McpServiceWrapper {
    Default(RunningService<RoleClient, InitializeRequestParams>),
    Channel(RunningService<RoleClient, Arc<ChannelHandler>>),
    #[cfg(test)]
    Controlled(ControlledMcpService),
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
    pub async fn close_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.close_with_timeout(timeout).await,
            McpServiceWrapper::Channel(svc) => svc.close_with_timeout(timeout).await,
            #[cfg(test)]
            McpServiceWrapper::Controlled(svc) => svc.close().await,
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        match self {
            McpServiceWrapper::Default(svc) => svc.peer(),
            McpServiceWrapper::Channel(svc) => svc.peer(),
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
