//! Agent 资源面**真实线路夹具**（W5）。
//!
//! 口径与 `builtin_subscription_workspace_wire_test.rs::connect_workspace` 一致：
//! **真实** `WorkspaceMcpServer`（生产 handler + 生产 provider 输入）+ **生产**
//! 链路装配（`spawn_builtin_transport_with_handler`）+ **生产** client 握手段
//! （`serve_client_auto`），句柄以 `ConfigSource::Builtin { instance: "workspace" }`
//! 注册进生产 `McpClientPool`。
//!
//! 与测试替身的分工：禁用替身读路径；本夹具让 Agent 目录投影（`resources/list`）
//! 与正文激活（`resources/read`）都走生产路径——`McpAgentRegistry` 的
//! entries/local_catalog/activate 与 SubAgentTool 的定义加载因此有真实证据。
//!
//! 夹具自带 runtime 并按序关闭（service → server supervisor 收敛），不依赖调用方
//! 的 tokio 运行时（`#[test]` 与 `#[tokio::test]` 都能用）。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::plugin::ConfigSource;
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceMcpServer, WorkspaceResourcesInput,
};

use super::apps::McpCapabilityProfile;
use super::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use super::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 一条已握手的 workspace 资源面线路（自带 runtime 与池）。
pub(crate) struct AgentFaceFixture {
    pub pool: Arc<McpClientPool>,
    pub registry: Arc<super::McpAgentRegistry>,
    #[allow(dead_code)]
    shutdown: Shutdown,
    service: Option<McpServiceWrapper>,
    supervisor: Option<BuiltinInstanceSupervisor>,
}

impl AgentFaceFixture {
    /// 连接一个带 **project agent 根**（`{cwd}/.claude/agents`）的 workspace 实例。
    ///
    /// 异步形态：在调用方 runtime 上建立线路（`#[tokio::test]` 直接 `.await`）；
    /// 同步用例用 [`AgentFaceFixture::connect_blocking`]。
    pub(crate) async fn connect(cwd: &Path) -> Self {
        let input = WorkspaceResourcesInput::new().with_agent_root(ResourceRoot::new(
            cwd.join(".claude").join("agents"),
            ResourceScope::Project,
        ));
        let (service, supervisor, pool) = async move {
            let transport = spawn_builtin_transport_with_handler(
                "workspace",
                WorkspaceMcpServer::new(cwd.to_string_lossy().as_ref(), None).with_resources(input),
            );
            let (io, supervisor) = transport.into_parts();
            let service =
                serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
                    .await
                    .expect("builtin 握手不得超时（同进程链路）")
                    .expect("builtin 握手不得失败");
            // 目录快照走生产 client 方法（与初始化期同一路径）；工具表本夹具不消费。
            let peer = service.peer().clone();
            let resources = peer.list_all_resources().await.expect("resources/list");
            let handle = Arc::new(McpClientHandle {
                name: "workspace".to_string(),
                version: None,
                cache_version: None,
                peer: Some(peer),
                tools: Vec::new(),
                resources,
                status: ClientStatus::Connected,
                oauth_status: Default::default(),
                source: Some(ConfigSource::Builtin {
                    instance: "workspace".to_string(),
                }),
                url: None,
                skills_capable: false,
            });
            let mut pool = McpClientPool::new_pending();
            pool.resource_cache = super::resource_cache::McpResourceCache::isolated_for_test();
            let pool = Arc::new(pool);
            pool.clients.write().insert("workspace".to_string(), handle);
            (service, supervisor, pool)
        }
        .await;
        let registry = Arc::new(super::McpAgentRegistry::new(Arc::clone(&pool)));
        Self {
            pool,
            registry,
            shutdown: Shutdown::OnCurrentRuntime,
            service: Some(service),
            supervisor: Some(supervisor),
        }
    }
}

/// 夹具关闭策略：借用调用方 runtime（异步用例）或自带 runtime（同步用例）。
enum Shutdown {
    OnCurrentRuntime,
}

impl Drop for AgentFaceFixture {
    fn drop(&mut self) {
        let (Some(mut service), Some(supervisor)) = (self.service.take(), self.supervisor.take())
        else {
            return;
        };
        // 先释放会保活 client 传输的引用（pool 的句柄）。
        self.pool.clients.write().clear();
        let shutdown = async move {
            let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
            let _ = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        };
        match self.shutdown {
            Shutdown::OnCurrentRuntime => {
                // 异步用例：关闭在调用方 runtime 上排队，不阻塞（fixture 随线程存活）。
                tokio::runtime::Handle::try_current()
                    .ok()
                    .map(|handle| handle.spawn(shutdown));
            }
        }
    }
}
