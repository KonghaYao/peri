//! MCP pool 的状态所有权、构造、基础查询与宿主端口。
//! 缓存、OAuth、生命周期及状态投影分别由私有子模块实现。

mod cache;
#[cfg(test)]
#[path = "client/cache_policy_test.rs"]
mod cache_policy_tests;
mod lifecycle;
mod oauth;
pub(crate) mod output_store;
#[cfg(not(target_os = "emscripten"))]
pub(crate) mod process;
#[cfg(target_os = "emscripten")]
#[path = "client/process_wasm.rs"]
pub(crate) mod process;
// System MCP 启动准入 seam：清单发布与闸门已接线（`initialize` / `middleware`），
// 仍有三处冻结但尚无生产读取方的成员——`DiscoveryEvidence::is_complete`（只有测试在问）、
// `NegotiatedSystemMcp::handle`（协商结果保留句柄，消费方只读 `requirement`/`generation`）、
// `SystemReadinessError::CatalogPublicationFailed`（清单发布失败分支待接）。三者都由
// crate 内测试覆盖，接线波次落地后连同本豁免一起删除。
#[allow(dead_code)]
mod readiness;
mod rewind;
mod service;
mod status;
mod subscription;
mod task_scope_metadata;
mod transport;
mod types;

use super::{
    builtin::{
        context::{BuiltinContextError, BuiltinInstanceContext},
        runtime::{BuiltinSpawnError, BuiltinTransport, TickGuard, BUILTIN_TICK_INTERVAL},
    },
    config::{McpCachePolicy, McpServerConfig},
    oauth_flow::OAuthFlowEvent,
};
use lifecycle::ServiceShutdownState;
use oauth::{OAuthFlowKey, PendingOAuthCallback};
use peri_acp_types::{
    builtin_mcp::find,
    command_registry::CommandRegistry,
    dynamic_mcp::DynamicMcpInstanceKey,
    mcp::McpSubscriptionPort,
    mcp_skills::McpSkillRegistry,
    ports::{
        McpBuiltinWorkspaceState, McpOAuthStartDisposition, McpPoolShutdownReport,
        McpServerConnectionStatus, McpServerInfo, McpServerOAuthStatus,
    },
    session::InboxHandle,
    skills::SkillMetadata,
    tasks::TaskManager,
};
use readiness::SystemReadinessTracker;
use rmcp::model::{Resource, ResourceContents, Tool};
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) use cache::cache_scope_allows_persistence;
pub use oauth::OAuthStartDisposition;
// System MCP 启动准入（IF-M3）：证据、等待与类型化错误；子模块声明留在本文件，
// 不占 `mcp/mod.rs`（其 owner 为 C-INJ-02 / D-02）。消费方：B-02（证据提交 /
// 清单发布，已接线）、B-03（闸门与 C 接线，W4 消费 `NegotiatedSystemMcp` /
// `SystemMcpRequirement` / `SystemReadinessError`）。
#[allow(unused_imports)]
pub(crate) use readiness::{
    DiscoveryEvidence, NegotiatedSystemMcp, SystemMcpManifest, SystemMcpRequirement,
    SystemReadinessError,
};
#[cfg(test)]
pub(crate) use service::ControlledMcpService;
pub(crate) use service::{
    mcpp_client_info_for_profile, peer_declares_skills, McpServiceOwner, McpServiceWrapper,
};
pub use status::redact_mcp_error;
#[cfg(test)]
pub(crate) use status::status_change_text;
#[cfg(test)]
use status::{mcp_error_summary, mcp_status_label};
#[cfg(test)]
pub(crate) use subscription::build_subscription_filter;
pub(crate) use subscription::setup_subscription;
pub(crate) use transport::{build_authed_transport, build_http_transport, serve_client_auto};
pub(crate) use types::McpConnectionKey;
pub use types::{
    ClientStatus, McpClientHandle, McpInitStatus, McpPoolError, OAuthStatus, ServerInfo,
};

/// MCP 客户端连接池
pub struct McpClientPool {
    cache_policy: std::sync::OnceLock<McpCachePolicy>,
    workspace_scope: std::sync::OnceLock<peri_acp_types::workspace::WorkspaceId>,
    pub(super) configuration_snapshot: std::sync::OnceLock<Arc<peri_config::ConfigurationSnapshot>>,
    pub(super) session_servers: std::sync::OnceLock<HashMap<String, McpServerConfig>>,
    credential_client: std::sync::OnceLock<super::auth_store::OAuthCredentialClient>,
    pub(super) builtin_available: std::sync::atomic::AtomicBool,
    pub(super) stdio_available: std::sync::atomic::AtomicBool,
    pub(super) plugin_discovery_available: std::sync::atomic::AtomicBool,
    shared_services: parking_lot::Mutex<Vec<Arc<McpServiceOwner>>>,
    /// Includes failed handshakes until their actual process tree and stderr have drained.
    #[cfg(not(target_os = "emscripten"))]
    processes: parking_lot::Mutex<Vec<Arc<process::McpProcessOwner>>>,
    /// Static transports reconnect in the same session directory used for initial discovery.
    pub(crate) execution_cwd: std::sync::OnceLock<std::path::PathBuf>,
    /// Pool-wide admission gate. 0=open, 1=closing, 2=closed.
    lifecycle: std::sync::atomic::AtomicU8,
    pub(crate) lifecycle_registration: parking_lot::Mutex<()>,
    /// Pool-owned terminal service-close transaction. Awaiting a borrowed
    /// handle is cancellation-safe: a dropped waiter cannot detach the worker
    /// or the drained services it owns.
    service_shutdown: tokio::sync::Mutex<ServiceShutdownState>,
    pub(crate) task_spawner: super::task_scope::McpTaskSpawner,
    pub(crate) clients: parking_lot::RwLock<HashMap<String, Arc<McpClientHandle>>>,
    handle_generations:
        parking_lot::Mutex<HashMap<String, Vec<(std::sync::Weak<McpClientHandle>, u64)>>>,
    next_handle_generation: std::sync::atomic::AtomicU64,
    pub(crate) services: parking_lot::Mutex<HashMap<String, McpServiceWrapper>>,
    /// builtin 实例的**代监督者**表（键 = server name，与 `services` 同期登记 / 移除）。
    ///
    /// 值是关闭所有权而不是裸 server task：builtin 的 server 半边是本进程内的 task，
    /// 关闭语义必须显式（有界等待 → 未收敛才 abort），且顺序固定（先停 tick 再收敛
    /// server task）。不能假设「client service 关闭后它自己会退出」（spike Q2 才有该结论）。
    /// 冻结：wave 1 **不**新增 `McpTaskKey` 变体，本表独立于 keyed task 作用域。
    pub(crate) builtin_server_tasks:
        parking_lot::Mutex<HashMap<String, super::builtin::runtime::BuiltinInstanceSupervisor>>,
    /// builtin 实例上下文槽（IF-P3-04 / A33）。
    ///
    /// 语义是**一次性注入**：宿主装配（`peri-acp`）在 `run_initialize` 之前注入，首次生效；
    /// 重复注入（含同一 `Arc` 再注入）与「initialize 已开始」后的晚注入都返回 typed 错误。
    /// 上下文与「initialize 已开始」标志由**同一把短锁**保护——锁内只做读判定与写入、
    /// 禁止 await，因此两个竞争注入恰好一个成功，且晚注入的判定与 `initialize` 封口
    /// 之间不存在窗口（`OnceLock::set` 表达不了后半条）。
    pub(crate) builtin_context: parking_lot::Mutex<BuiltinContextSlot>,
    pub(crate) configs: parking_lot::RwLock<HashMap<String, McpServerConfig>>,
    pub(crate) cache_versions: parking_lot::RwLock<HashMap<String, String>>,
    /// 插件来源旁路表：key 为 server name（如 `"plugin:p1:srv1"`），value 为 `"name@marketplace"`
    pub(crate) plugin_sources: parking_lot::RwLock<HashMap<String, String>>,
    /// 初始化阶段内部存储（M-TUI 收口：TUI 不再持有 watch channel，`mcp/list`
    /// 命令面经 `McpPoolPort::snapshot` 读取；`run_initialize` 与外部
    /// `status_tx` 同步更新）。
    pub(crate) init_status: parking_lot::RwLock<McpInitStatus>,
    /// 初始化是否已完成。完成前发生的状态写入**不**产生上下线通知——
    /// 会话首 turn 的 `first_turn_reminder` 概览已覆盖初始连接结果，避免与
    /// 逐台上线事件重复（初始化未完成时，迟到的连接成功自然成为运行中变化）。
    pub(crate) initialized: std::sync::atomic::AtomicBool,
    /// 运行中状态变化的待注入文本缓冲（McpMiddleware::before_model drain 后
    /// 以 Info 消息推送进模型上下文；全局缓冲，任一会话消费一次即清空）。
    pub(crate) pending_changes: parking_lot::Mutex<Vec<String>>,
    /// 状态变化通知回调（装配时注入；发布 system-notification 给 TUI 通知面）。
    notifier: parking_lot::RwLock<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    /// OAuth 流程事件回调（装配时注入；`AuthorizationNeeded` 需把
    /// `callback_tx` 注册进 `pending_oauth_callbacks` 供授权码回传 RPC 投递，
    /// 其余事件转发为 ACP `oauth-needed` / `oauth-completed` / `oauth-failed`）。
    oauth_event_callback: parking_lot::RwLock<Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>>>,
    /// 待完成 OAuth 授权的回调通道。物理连接与 flow identity 共同定位，
    /// dynamic 路径不得降维为裸 server name。
    pending_oauth_callbacks: parking_lot::Mutex<HashMap<OAuthFlowKey, PendingOAuthCallback>>,
    /// 每个 scoped connection 最多一个活跃 OAuth flow。
    active_oauth_flows: parking_lot::Mutex<HashMap<McpConnectionKey, String>>,
    /// subscriptions/listen 会话 inbox 注册表（session_id → InboxHandle）。
    /// SessionManager（peri-acp）经 `McpSubscriptionPort` 注册；订阅通知到达
    /// 时向全部注册 inbox 推送 Defer 消息并唤醒 idle agent。
    pub(crate) session_inboxes: parking_lot::RwLock<HashMap<String, InboxHandle>>,
    /// One session runtime handle per session; task records remain owned by Agent.
    pub(crate) session_tasks:
        parking_lot::RwLock<HashMap<String, Arc<dyn peri_acp_types::tasks::TaskManager>>>,
    pub(crate) task_scope_authority: peri_mcp_core::task_scope::TaskScopeAuthority,
    pub(crate) task_scope_tokens: parking_lot::RwLock<HashMap<String, String>>,
    /// 跨进程的 MCP Resource Cache；是否写入由响应 scope 与安全上下文共同决定。
    pub(crate) resource_cache: super::resource_cache::McpResourceCache,
    /// 进程启动时冻结的 deployment capability profile；初始连接和重连复用。
    pub(crate) capability_profile: super::apps::McpCapabilityProfile,
    /// 初始模型 MCP tool invocation 签发、`peri/mcp/open` 单次消费的租约。
    pub(crate) app_binding_leases: Arc<super::apps::McpAppBindingLeaseRegistry>,
    /// System MCP 启动准入事实（IF-M3）：配置清单状态 + 每台 server 的本代发现
    /// 证据 + 独立 watch revision。所有写入都必须经由成功/失败的生产路径。
    pub(crate) system_readiness: SystemReadinessTracker,
    /// 会话级 ACP（MCP over ACP）连接归属：池内 server name → 所属 session id。
    /// 无条目的 server 对所有会话可见（配置来源与 dynamic 投影的既有语义）；
    /// 有条目的仅在归属会话内可见（工具桥接与状态面据此过滤）。
    pub(crate) acp_owners: parking_lot::RwLock<HashMap<String, String>>,
}

/// builtin 实例上下文槽（A33）：上下文与「initialize 已开始」标志由**同一短锁**保护
/// （锁内禁 await）。
///
/// 两件事必须同锁：否则「判重复 → 写上下文」与「封口 → 判晚注入」之间存在竞态，晚注入
/// 可能挤进已经开始的 initialize。`initialize_started` 一旦置真即不可回退（seal 幂等，
/// 本类型不提供解封口入口）。
pub(crate) struct BuiltinContextSlot {
    /// 已注入的上下文；`None` = 尚未注入（**不是**「不需要上下文」）。
    context: Option<Arc<BuiltinInstanceContext>>,
    /// `initialize` 是否已开始（seal 点见 `initialize::run_initialize` /
    /// `initialize::initialize_config` 的函数体首行）。
    pub(super) initialize_started: bool,
}

pub(crate) const STDIO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl McpClientPool {
    pub fn new_pending() -> Self {
        Self::new_pending_with_spawner(super::task_scope::McpTaskSpawner::closed())
    }

    pub fn new_pending_with_spawner(spawner: super::task_scope::McpTaskSpawner) -> Self {
        Self::new_pending_with_spawner_and_profile(
            spawner,
            super::apps::McpCapabilityProfile::disabled(),
        )
    }

    pub fn new_pending_with_spawner_and_profile(
        spawner: super::task_scope::McpTaskSpawner,
        capability_profile: super::apps::McpCapabilityProfile,
    ) -> Self {
        Self {
            cache_policy: std::sync::OnceLock::new(),
            workspace_scope: std::sync::OnceLock::new(),
            configuration_snapshot: std::sync::OnceLock::new(),
            session_servers: std::sync::OnceLock::new(),
            credential_client: std::sync::OnceLock::new(),
            builtin_available: std::sync::atomic::AtomicBool::new(true),
            stdio_available: std::sync::atomic::AtomicBool::new(crate::platform::STDIO_TRANSPORT),
            plugin_discovery_available: std::sync::atomic::AtomicBool::new(true),
            shared_services: parking_lot::Mutex::new(Vec::new()),
            #[cfg(not(target_os = "emscripten"))]
            processes: parking_lot::Mutex::new(Vec::new()),
            execution_cwd: std::sync::OnceLock::new(),
            lifecycle: std::sync::atomic::AtomicU8::new(0),
            lifecycle_registration: parking_lot::Mutex::new(()),
            service_shutdown: tokio::sync::Mutex::new(ServiceShutdownState::Idle),
            task_spawner: spawner,
            clients: parking_lot::RwLock::new(HashMap::new()),
            handle_generations: parking_lot::Mutex::new(HashMap::new()),
            next_handle_generation: std::sync::atomic::AtomicU64::new(1),
            services: parking_lot::Mutex::new(HashMap::new()),
            builtin_server_tasks: parking_lot::Mutex::new(HashMap::new()),
            builtin_context: parking_lot::Mutex::new(BuiltinContextSlot {
                context: None,
                initialize_started: false,
            }),
            configs: parking_lot::RwLock::new(HashMap::new()),
            cache_versions: parking_lot::RwLock::new(HashMap::new()),
            plugin_sources: parking_lot::RwLock::new(HashMap::new()),
            init_status: parking_lot::RwLock::new(McpInitStatus::Pending),
            initialized: std::sync::atomic::AtomicBool::new(false),
            pending_changes: parking_lot::Mutex::new(Vec::new()),
            notifier: parking_lot::RwLock::new(None),
            oauth_event_callback: parking_lot::RwLock::new(None),
            pending_oauth_callbacks: parking_lot::Mutex::new(HashMap::new()),
            active_oauth_flows: parking_lot::Mutex::new(HashMap::new()),
            session_inboxes: parking_lot::RwLock::new(HashMap::new()),
            session_tasks: parking_lot::RwLock::new(HashMap::new()),
            task_scope_authority: peri_mcp_core::task_scope::TaskScopeAuthority::new(),
            task_scope_tokens: parking_lot::RwLock::new(HashMap::new()),
            resource_cache: super::resource_cache::McpResourceCache::new(),
            capability_profile,
            app_binding_leases: Arc::new(super::apps::McpAppBindingLeaseRegistry::default()),
            system_readiness: SystemReadinessTracker::new(),
            acp_owners: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    pub fn bind_execution_cwd(&self, cwd: &std::path::Path) -> std::io::Result<&std::path::Path> {
        let result = (|| {
            let cwd = std::path::absolute(cwd)?;
            let stored = self.execution_cwd.get_or_init(|| cwd.clone());
            if stored != &cwd {
                return Err(std::io::Error::other(
                    "MCP pool cannot change its execution directory",
                ));
            }
            Ok(stored.as_path())
        })();
        if let Err(error) = &result {
            *self.init_status.write() = McpInitStatus::Failed(error.to_string());
        }
        result
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self::new_empty_with_cache_policy(McpCachePolicy::Enabled)
    }

    #[cfg(test)]
    pub(crate) fn new_empty_with_cache_policy(policy: McpCachePolicy) -> Self {
        let mut pool = Self::new_pending();
        pool.bind_workspace_scope(peri_acp_types::workspace::WorkspaceId::new())
            .unwrap();
        pool.bind_cache_policy(policy).unwrap();
        pool.resource_cache = super::resource_cache::McpResourceCache::isolated_for_test();
        pool
    }

    pub fn set_configuration_snapshot(
        &self,
        snapshot: Arc<peri_config::ConfigurationSnapshot>,
    ) -> std::io::Result<()> {
        let context = self.builtin_context.lock();
        if context.initialize_started || !self.is_open() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "configuration snapshot must be bound before MCP initialization",
            ));
        }
        self.configuration_snapshot.set(snapshot).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "MCP configuration snapshot is already bound",
            )
        })
    }

    /// Bind persistent MCP data to the saved Workspace before initialization.
    pub fn bind_workspace_scope(
        &self,
        workspace_id: peri_acp_types::workspace::WorkspaceId,
    ) -> std::io::Result<()> {
        let context = self.builtin_context.lock();
        if context.initialize_started || !self.is_open() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workspace scope must be bound before MCP initialization",
            ));
        }
        self.workspace_scope.set(workspace_id).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "MCP workspace scope is already bound",
            )
        })
    }

    /// ACP session setup declarations are scoped to this pool and override file configuration.
    pub fn set_session_servers(
        &self,
        servers: HashMap<String, McpServerConfig>,
    ) -> std::io::Result<()> {
        let context = self.builtin_context.lock();
        if context.initialize_started || !self.is_open() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "session MCP servers must be bound before initialization",
            ));
        }
        self.session_servers.set(servers).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "session MCP servers already bound",
            )
        })
    }

    pub fn configuration_revision(&self) -> Option<peri_config::ConfigurationRevision> {
        self.configuration_snapshot
            .get()
            .map(|snapshot| snapshot.revision())
    }

    /// Reserve session execution before a tool call can create an MCP Task.
    /// Unconfirmed response loss leaves shutdown incomplete.
    pub(crate) fn begin_external_task_execution(
        &self,
        session_id: &str,
        scope: &str,
    ) -> Result<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>, String> {
        self.session_tasks
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "session task manager unavailable".to_owned())?
            .begin_external_execution(scope)
    }

    /// Report whether this session can have asynchronous work owned outside
    /// the trusted Workspace task scope. Read after initialization so failed
    /// and disconnected configured servers remain in the catalog.
    pub async fn has_unsupported_async_task_owner(
        &self,
        trusted_workspace_endpoint: Option<&str>,
    ) -> Result<bool, String> {
        let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(10);
        while !self.initialized.load(std::sync::atomic::Ordering::Acquire) {
            if peri_time::monotonic_now() >= deadline {
                return Err("MCP owner catalog did not initialize".into());
            }
            peri_time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let configs = self.configs.read();
        Ok(configs.values().any(|config| {
            if config.disabled == Some(true) {
                return false;
            }
            match config.source.as_ref() {
                Some(peri_acp_types::plugin::ConfigSource::Builtin { .. }) => false,
                Some(peri_acp_types::plugin::ConfigSource::WorkspaceRemote) => {
                    config.oauth.is_some() || config.url.as_deref() != trusted_workspace_endpoint
                }
                _ => true,
            }
        }))
    }

    pub(super) fn bind_cache_policy(&self, policy: McpCachePolicy) -> std::io::Result<()> {
        let stored = self.cache_policy.get_or_init(|| policy);
        if *stored != policy {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "MCP pool cannot change its cache policy",
            ));
        }
        Ok(())
    }

    /// 一次性注入 builtin 实例上下文（IF-P3-04 / A33）：首次生效，失败**不覆盖**既有
    /// 上下文（错误只报「为什么这一次没生效」）。
    ///
    /// 拒绝语义（同一短锁内判定，锁内不 await）：
    /// - `initialize` 已开始（[`Self::seal_builtin_context`] 已调用）→
    ///   [`BuiltinContextError::InitializationStarted`]：晚于配置加载窗口的注入无法影响
    ///   本轮实例装配，接受它只会制造「上下文在，但实例是按旧上下文建的」的假象；
    /// - 已有上下文（含传入**同一** `Arc` 的再注入）→
    ///   [`BuiltinContextError::AlreadyInjected`]：注入是宿主组合根的单一动作，重复注入
    ///   只可能是双装配 bug，必须可见而不是静默覆盖。
    pub fn set_builtin_instance_context(
        &self,
        context: Arc<BuiltinInstanceContext>,
    ) -> Result<(), BuiltinContextError> {
        let mut slot = self.builtin_context.lock();
        if slot.initialize_started {
            return Err(BuiltinContextError::InitializationStarted);
        }
        if slot.context.is_some() {
            return Err(BuiltinContextError::AlreadyInjected);
        }
        let _ = context
            .task_scope_authority
            .set(self.task_scope_authority.clone());
        slot.context = Some(context);
        Ok(())
    }

    /// 封口：标记「`initialize` 已开始」，此后首次注入一律被拒绝（幂等）。
    ///
    /// 调用点只有两个（`initialize` 的形状约束），见 `initialize::run_initialize` 与
    /// `initialize::initialize_config` 的函数体首行。
    pub(crate) fn seal_builtin_context(&self) {
        self.builtin_context.lock().initialize_started = true;
    }

    /// 已注入的上下文（锁内 clone `Arc`；未注入为 `None`）。
    pub(crate) fn builtin_instance_context(&self) -> Option<Arc<BuiltinInstanceContext>> {
        self.builtin_context.lock().context.clone()
    }

    /// 订阅建立门（A24 关闭集；判定只走 `builtin::is_closed`）。
    ///
    /// `closed` 取自宿主经 [`BuiltinInstanceContext`] 注入的 A24 关闭集（会话装配从
    /// frozen meta-harness 派生）：命中的 builtin 实例**不建立** `subscriptions/listen`
    /// 长流。订阅是能力的外部副作用（服务端据此才采样 git），关闭语义必须覆盖它
    /// （ARC-CAPABILITY-CLOSURE-001；粗粒度关闭 = `WorkspaceMiddleware: false`）。
    ///
    /// 未注入上下文 / 非 builtin 名字（关闭集只含 builtin 实例名）⇒ 恒允许，既有外部
    /// server 的订阅行为逐位不变。
    pub(crate) fn subscription_allowed(&self, server: &str) -> bool {
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        let source = self
            .configs
            .read()
            .get(server)
            .and_then(|config| config.source.clone());
        !super::builtin::is_closed_source(server, source.as_ref(), &closed)
    }

    fn workspace_source_closed(&self, closed: &std::collections::BTreeSet<String>) -> bool {
        let source = self
            .get_client("workspace")
            .and_then(|handle| handle.source.clone());
        super::builtin::is_closed_source("workspace", source.as_ref(), closed)
    }

    /// builtin 实例的**唯一** spawn 点（IF-P3-04 / A32）：实例解析 → 上下文 → 链路
    /// 装配 → tick。`initialize` 与 `reconnect` 都只调它，不各自读 cwd / 上下文。
    ///
    /// 顺序固定，每一步的错误都不得被后一步掩盖：
    /// 1. **先解析实例**：未注册实例一律 typed `UnknownInstance`（与 stdio / http 路径的
    ///    拒绝语义同源），即使此刻上下文缺失也不改报 `ContextMissing`；
    /// 2. 取上下文：未注入 ⇒ `ContextMissing`（不 panic、不降级成别的传输形态）；
    /// 3. [`super::builtin::runtime::spawn_builtin_transport_with_context`] 装配：
    ///    「已注册但缺该类输入」与「handler 尚未接线」是两个不同的 typed 原因；
    /// 4. tick：仅当注册表名是 `cron`、上下文带 `cron` 输入**且** `tick_enabled` 为真时，
    ///    为**本代** transport spawn 一个 tick（每 `BUILTIN_TICK_INTERVAL` 一次
    ///    `scheduler.lock().tick()`，语义与既有宿主 `HostTaskKind::CronTick` 一致）。
    ///    这是全仓唯一的 tick spawn 点：handler 不持有 tick，tick 随本代监督者一起关闭。
    #[cfg(test)]
    pub(crate) fn spawn_builtin_transport(
        &self,
        instance: &str,
    ) -> Result<BuiltinTransport, BuiltinSpawnError> {
        self.spawn_builtin_transport_with_environment(instance, &std::collections::HashMap::new())
    }

    pub(crate) fn spawn_builtin_transport_with_environment(
        &self,
        instance: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<BuiltinTransport, BuiltinSpawnError> {
        super::transport::require_known_builtin_instance(instance).map_err(|_| {
            BuiltinSpawnError::UnknownInstance {
                instance: instance.to_string(),
            }
        })?;
        let context =
            self.builtin_instance_context()
                .ok_or_else(|| BuiltinSpawnError::ContextMissing {
                    instance: instance.to_string(),
                })?;
        let mut transport =
            super::builtin::runtime::spawn_builtin_transport_with_context(instance, &context, env)?;
        // 注册表名判定（不写第二张实例名字表）：只有 `cron` 有 tick 驱动语义。
        #[cfg(not(target_os = "emscripten"))]
        if find(instance).is_some_and(|registered| registered.name == "cron") {
            if let Some(cron) = context.cron.as_ref().filter(|cron| cron.tick_enabled) {
                let scheduler = Arc::clone(&cron.scheduler);
                transport.tick = Some(TickGuard::spawn(BUILTIN_TICK_INTERVAL, move || {
                    scheduler.lock().tick();
                }));
            }
        }
        Ok(transport)
    }

    /// 查询指定 server 的插件来源标识，非插件 server 返回 None
    /// key 格式为 `"plugin_name__server_name"`，返回 `"name@marketplace"`
    pub fn plugin_source_of(&self, name: &str) -> Option<String> {
        self.plugin_sources.read().get(name).cloned()
    }

    /// MetaHarness 段落覆盖读取（J6）：只认真实 builtin `workspace` 实例。
    ///
    /// - X7：来源白名单按实例身份（`ConfigSource::Builtin { instance: "workspace" }`），
    ///   外部 server 的同 scheme 资源一律不进入扫描（不按 scheme 信任）；
    /// - X8：关闭集命中的实例直接返回空批（覆盖不可用 ⇒ 宿主保持内置段落；
    ///   无磁盘兜底，宿主已无 FS 扫描点）；
    /// - 未连接 / 未注入上下文同样返回空批，由宿主 X8 语义处理。
    pub async fn read_builtin_workspace_meta(
        &self,
        enabled_sections: &std::collections::HashSet<String>,
    ) -> Result<HashMap<String, String>, String> {
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        if self.workspace_source_closed(&closed) {
            tracing::warn!("meta_harness: builtin workspace is closed; keeping builtin sections");
            return Ok(HashMap::new());
        }
        let Some(handle) = self.get_client("workspace") else {
            return Ok(HashMap::new());
        };
        if !matches!(handle.status, ClientStatus::Connected)
            || !super::builtin::is_workspace_source(handle.source.as_ref())
        {
            return Ok(HashMap::new());
        }
        let Some(peer) = handle.peer.as_ref() else {
            return Ok(HashMap::new());
        };
        let resources = self
            .list_all_resources_cached("workspace", peer)
            .await
            .map_err(|error| format!("resources/list failed: {error}"))?;
        let mut docs = HashMap::new();
        for resource in resources {
            let Some(meta) = peri_acp_types::workspace_resources::parse_meta_uri(&resource.uri)
            else {
                continue;
            };
            if !enabled_sections.contains(&meta.section_id)
                || !peri_acp_types::meta_harness::SECTION_IDS.contains(&meta.section_id.as_str())
            {
                continue;
            }
            let (result, ticket) = self
                .read_resource_cached("workspace", &resource.uri, peer)
                .await
                .map_err(|error| format!("resources/read failed: {error}"))?;
            self.cache_verified_resource("workspace", ticket, &result)
                .await;
            if let Some(text) = result.contents.iter().find_map(|content| match content {
                ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
                _ => None,
            }) {
                docs.insert(meta.section_id, text);
            }
        }
        Ok(docs)
    }

    /// 冻结期项目指令读取（W5/E15）：只认真实 builtin `workspace` 实例。
    ///
    /// 身份与关闭口径与 [`Self::read_builtin_workspace_meta`] 同源（实例绑定 +
    /// Connected + A24 关闭集）；`disableBundledSkills` 等 skill 面开关不适用于
    /// 指令面。
    ///
    /// 失败语义（X5，受限 Peri profile）：
    /// - 未装配 / 实例被关闭 / 未连接 / 非本机 builtin 实例 ⇒ `Ok((None, None))`
    ///   ：指令面不适用，**不是失败**，也不回落磁盘（X4/J5）；
    /// - 实例健康但 `resources/list` / `resources/read` 失败 ⇒ `Err`：system
    ///   已声明且被选中的投递失败，调用方按 J2 补偿 fail-closed；
    /// - 文档不存在（`resources/list` 未列 main/local）⇒ `Ok(None)`，不是错误。
    ///
    /// 返回 `(main, local)`：main 为 import 展开后的正文（provider 侧解析），
    /// local 为原文；两者都可为 `None`。
    pub async fn read_builtin_workspace_instructions(
        &self,
    ) -> Result<(Option<String>, Option<String>), String> {
        use peri_acp_types::workspace_resources::{
            parse_instruction_uri, INSTRUCTION_LOCAL_URI, INSTRUCTION_MAIN_URI,
        };
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        if self.workspace_source_closed(&closed) {
            tracing::debug!(
                "instructions: builtin workspace is closed; instruction face stays unavailable"
            );
            return Ok((None, None));
        }
        let Some(handle) = self.get_client("workspace") else {
            return Ok((None, None));
        };
        if !matches!(handle.status, ClientStatus::Connected)
            || !super::builtin::is_workspace_source(handle.source.as_ref())
        {
            return Ok((None, None));
        }
        let Some(peer) = handle.peer.clone() else {
            return Err(
                "builtin workspace instruction face is connected but the peer is unavailable"
                    .to_string(),
            );
        };
        let resources = self
            .list_all_resources_cached("workspace", &peer)
            .await
            .map_err(|error| format!("resources/list failed: {error}"))?;
        let mut main: Option<String> = None;
        let mut local: Option<String> = None;
        for resource in resources {
            // 只读固定的两条指令 URI（不按 scheme 泛化：其余 scheme 不在此面）。
            let uri = resource.uri.as_str();
            let is_main = uri == INSTRUCTION_MAIN_URI;
            let is_local = uri == INSTRUCTION_LOCAL_URI;
            if !is_main && !is_local {
                continue;
            }
            if parse_instruction_uri(uri).is_none() {
                continue;
            }
            let (result, ticket) = self
                .read_resource_cached("workspace", uri, &peer)
                .await
                .map_err(|error| format!("resources/read failed: {error}"))?;
            self.cache_verified_resource("workspace", ticket, &result)
                .await;
            let Some(text) = result.contents.iter().find_map(|content| match content {
                ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
                _ => None,
            }) else {
                continue;
            };
            match (is_main, is_local) {
                (true, _) => main = Some(text),
                (_, true) => local = Some(text),
                _ => {}
            }
        }
        Ok((main, local))
    }

    /// 冻结期技能清单快照（F3，J1/§5.4）：只认真实 builtin `workspace` 实例。
    ///
    /// 身份与关闭口径与 [`Self::read_builtin_workspace_meta`] 同源（X6/X7：
    /// `ConfigSource::Builtin { instance: "workspace" }` + Connected；X4/A24：
    /// 关闭集命中直接返回空，不发现、不回落磁盘）。
    ///
    /// 失败语义（X5，受限 Peri profile）：
    /// - 实例未装配 / 未连接 / 未声明 skills 能力 → `Ok(空)`：**未声明不是失败**
    ///   （不凭空要求每台 server 支持 skills）；
    /// - 能力已声明且实例健康，但 `skills/list` 读取失败（超时 / RPC 错误 /
    ///   响应不合法）→ `Err`：system 已声明且被选中的投递失败 ⇒ 调用方
    ///   fail-closed（拒绝创建会话，不谎称「无技能」）。
    ///
    /// 返回的是**元数据快照**（发现面同构条目，正文不读）：正文由激活面
    /// （`resources/read` + digest 校验）按需读取。
    pub async fn read_builtin_workspace_skills(
        &self,
    ) -> Result<Vec<peri_acp_types::skills::SkillMetadata>, String> {
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        if self.workspace_source_closed(&closed) {
            tracing::debug!("skills: builtin workspace is closed; skill face stays unavailable");
            return Ok(Vec::new());
        }
        let Some(handle) = self.get_client("workspace") else {
            return Ok(Vec::new());
        };
        if !matches!(handle.status, ClientStatus::Connected)
            || !super::builtin::is_workspace_source(handle.source.as_ref())
        {
            return Ok(Vec::new());
        }
        if !handle.skills_capable {
            // 能力未声明 ⇒ 技能面不适用（X5）；不调用未声明的方法。
            return Ok(Vec::new());
        }
        let Some(peer) = handle.peer.clone() else {
            return Err(
                "builtin workspace skills face declared but the peer is unavailable".to_string(),
            );
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        super::skill_discovery::snapshot_via_skills_list(peer, &handle.name, cancel).await
    }

    pub fn get_tools(&self, name: &str) -> Vec<Tool> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.tools.clone())
            .unwrap_or_default()
    }
    pub fn get_resources(&self, name: &str) -> Vec<Resource> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.resources.clone())
            .unwrap_or_default()
    }
    pub fn get_client(&self, name: &str) -> Option<Arc<McpClientHandle>> {
        self.clients.read().get(name).cloned()
    }
    /// 会话可见的连接句柄（[`Self::get_client`] 的 ACP 归属过滤版）。
    pub fn get_client_visible_to(
        &self,
        name: &str,
        session_id: Option<&str>,
    ) -> Option<Arc<McpClientHandle>> {
        if session_id.is_some_and(|session_id| !self.is_visible_to_session(name, session_id)) {
            return None;
        }
        self.get_client(name)
    }
    pub fn get_all_clients(&self) -> Vec<Arc<McpClientHandle>> {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected))
            .cloned()
            .collect()
    }
    /// 会话可见的连接句柄（[`Self::get_all_clients`] 的 ACP 归属过滤版）。
    ///
    /// `session_id` 为 `None` 表示不过滤（部署面视图：面板、命令面、池关闭）。
    pub fn get_all_clients_visible_to(
        &self,
        session_id: Option<&str>,
    ) -> Vec<Arc<McpClientHandle>> {
        let mut clients = self.get_all_clients();
        if let Some(session_id) = session_id {
            clients.retain(|client| self.is_visible_to_session(&client.name, session_id));
        }
        clients
    }
    pub fn has_resources(&self) -> bool {
        self.clients
            .read()
            .values()
            .any(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
    }
    pub fn resource_summary(&self) -> String {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
            .map(|c| {
                format!(
                    "- server \"{}\": {} ({} resources)",
                    c.name,
                    c.resources
                        .iter()
                        .map(|r| r.uri.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    c.resources.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn rewind_files(
        &self,
        session_id: &str,
        changes: serde_json::Value,
    ) -> Result<(), String> {
        self.rewind_workspace_files(session_id, changes).await
    }

    fn has_active_tasks(&self, session_id: &str) -> bool {
        self.session_tasks
            .read()
            .get(session_id)
            .cloned()
            .is_some_and(|manager| manager.has_unsettled_external())
    }

    fn bind_agent_session(
        &self,
        session_id: &str,
        inbox: InboxHandle,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) {
        self.bind_session_task_manager(session_id, &manager);
        self.register_inbox(session_id, inbox);
    }

    fn agent_session_binding(
        &self,
        session_id: &str,
    ) -> Option<(InboxHandle, Arc<dyn peri_acp_types::tasks::TaskManager>)> {
        let inbox = self.session_inboxes.read().get(session_id).cloned()?;
        let manager = self.session_tasks.read().get(session_id).cloned()?;
        Some((inbox, manager))
    }

    fn begin_shutdown(&self) {
        McpClientPool::begin_shutdown(self);
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpClientPool::shutdown(self).await
    }

    fn snapshot(&self) -> serde_json::Value {
        let init_phase = match &*self.init_status.read() {
            McpInitStatus::Pending => "pending",
            McpInitStatus::Initializing { .. } => "initializing",
            McpInitStatus::Ready { .. } => "ready",
            McpInitStatus::Failed(_) => "failed",
        };
        let infos = self.all_server_infos();
        serde_json::json!({
            "initPhase": init_phase,
            "servers": infos.iter().map(|info| serde_json::json!({
                "name": info.name.clone(),
                "status": format!("{:?}", info.status).to_lowercase(),
                "transport": info.transport_type.clone(),
                "toolsCount": info.tool_count,
            })).collect::<Vec<_>>(),
        })
    }

    // ── W3 端口补全：委托既有固有方法，语义逐项对齐（见 ports.rs 契约文档）──

    fn server_infos(&self) -> Result<Vec<McpServerInfo>, String> {
        Ok(McpClientPool::all_server_infos(self)
            .into_iter()
            .map(mcp_server_info_projection)
            .collect())
    }

    fn active_oauth_flow(&self, server_name: &str) -> Option<String> {
        McpClientPool::active_oauth_flow(self, server_name)
    }

    fn spawn_oauth_flow_with_id(
        self: Arc<Self>,
        server_name: &str,
        flow_id: &str,
    ) -> Result<McpOAuthStartDisposition, String> {
        Ok(
            match McpClientPool::spawn_oauth_flow_with_id(&self, server_name, flow_id) {
                OAuthStartDisposition::Started => McpOAuthStartDisposition::Started,
                OAuthStartDisposition::AlreadyActive => McpOAuthStartDisposition::AlreadyActive,
                OAuthStartDisposition::Conflict { active_flow_id } => {
                    McpOAuthStartDisposition::Conflict { active_flow_id }
                }
            },
        )
    }

    fn deliver_oauth_callback(
        &self,
        server_name: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        McpClientPool::deliver_oauth_callback(self, server_name, code, state)
    }

    fn deliver_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        McpClientPool::deliver_dynamic_oauth_callback(self, instance, flow_id, code, state)
    }

    fn cancel_oauth_callback(&self, server_name: &str) -> Result<bool, String> {
        Ok(McpClientPool::cancel_oauth_callback(self, server_name))
    }

    fn cancel_dynamic_oauth_flow(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
    ) -> Result<bool, String> {
        Ok(McpClientPool::cancel_dynamic_oauth_flow(
            self, instance, flow_id,
        ))
    }

    async fn open_workspace_task_scope(&self, session_id: &str) -> Result<(), String> {
        McpClientPool::open_workspace_task_scope(self, session_id).await
    }

    async fn close_workspace_task_scope(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        McpClientPool::close_workspace_task_scope(&self, session_id).await
    }

    async fn reconcile_closing_workspace_scope(&self, session_id: &str) -> Result<(), String> {
        McpClientPool::reconcile_closing_workspace_scope(self, session_id).await
    }

    fn bind_session_task_manager(&self, session_id: &str, manager: &Arc<dyn TaskManager>) {
        McpClientPool::bind_session_task_manager(self, session_id, manager);
    }

    async fn recover_workspace_tasks(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        McpClientPool::recover_workspace_tasks(&self, session_id).await
    }

    async fn watch_workspace_tasks(
        self: Arc<Self>,
        session_id: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        McpClientPool::watch_workspace_tasks(&self, session_id, cancel).await
    }

    fn attach_connection_notifier(
        self: Arc<Self>,
        registry: Option<&Arc<McpSkillRegistry>>,
        command_registry: Option<&Arc<CommandRegistry>>,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        super::middleware::attach_connection_notifier(
            &self,
            registry,
            command_registry,
            cancel,
            None,
        );
    }

    fn prewarm_discovery(
        self: Arc<Self>,
        registry: &Arc<McpSkillRegistry>,
        command_registry: &Arc<CommandRegistry>,
        session_id: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        super::middleware::prewarm_discovery(&self, registry, command_registry, session_id, cancel);
    }

    fn builtin_workspace_state(&self) -> McpBuiltinWorkspaceState {
        match McpClientPool::get_client(self, "workspace") {
            None => McpBuiltinWorkspaceState::Absent,
            Some(handle) if matches!(handle.status, ClientStatus::Connected) => {
                McpBuiltinWorkspaceState::Connected
            }
            Some(_) => McpBuiltinWorkspaceState::NotConnected,
        }
    }

    async fn read_builtin_workspace_skills(&self) -> Result<Vec<SkillMetadata>, String> {
        McpClientPool::read_builtin_workspace_skills(self).await
    }

    async fn read_builtin_workspace_instructions(
        &self,
    ) -> Result<(Option<String>, Option<String>), String> {
        McpClientPool::read_builtin_workspace_instructions(self).await
    }

    async fn read_builtin_workspace_meta(
        &self,
        enabled_sections: &HashSet<String>,
    ) -> Result<HashMap<String, String>, String> {
        McpClientPool::read_builtin_workspace_meta(self, enabled_sections).await
    }
}

/// `mcp/list` 契约投影：只暴露面板需要的状态分类，屏蔽 `Failed` 的错误正文
/// 与其余内部字段（与 `peri-acp` 原 downcast 后逐字段映射一致）。
fn mcp_server_info_projection(info: ServerInfo) -> McpServerInfo {
    McpServerInfo {
        name: info.name,
        transport: info.transport_type,
        status: match info.status {
            ClientStatus::Connected => McpServerConnectionStatus::Connected,
            ClientStatus::Failed(_) => McpServerConnectionStatus::Failed,
            ClientStatus::Disconnected => McpServerConnectionStatus::Disconnected,
            ClientStatus::Disabled => McpServerConnectionStatus::Disabled,
            ClientStatus::Uninitialized => McpServerConnectionStatus::Uninitialized,
        },
        oauth_status: match info.oauth_status {
            OAuthStatus::None => McpServerOAuthStatus::None,
            OAuthStatus::Authorized => McpServerOAuthStatus::Authorized,
            OAuthStatus::NeedsAuthorization => McpServerOAuthStatus::NeedsAuthorization,
        },
        tool_count: info.tool_count,
        resource_count: info.resource_count,
    }
}

/// `McpSubscriptionPort` 实现：SessionManager（peri-acp）在 session 创建 /
/// 销毁时注册 / 注销 inbox；订阅通知到达时经 inbox 唤醒 agent。
impl McpSubscriptionPort for McpClientPool {
    fn register_inbox(&self, session_id: &str, handle: InboxHandle) {
        self.session_inboxes
            .write()
            .insert(session_id.to_string(), handle);
    }

    fn unregister_inbox(&self, session_id: &str) {
        self.session_inboxes.write().remove(session_id);
        self.session_tasks.write().remove(session_id);
        self.task_scope_tokens.write().remove(session_id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
