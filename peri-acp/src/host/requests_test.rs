use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

use crate::provider::{PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use async_trait::async_trait;
use peri_acp_types::event_data::PluginSnapshotEntry;
use peri_acp_types::plugin::{InstallScope, InstalledPlugin, PluginManagerPort, PluginOrigin};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::session_resources::{
    BindingState, FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::{PersistedPayload, ThreadStore};
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::workspace::SessionBinding;
use peri_middlewares::permission::shared_mode::{PermissionMode, SharedPermissionMode};
use peri_middlewares::workflow::WorkflowMiddleware;
use peri_resources::sessions::SqliteThreadStore;
use peri_workflow::protocol::{AgentRunParams, AgentRunResult, Usage};
use peri_workflow::registry::{WorkflowRun, WorkflowRunStatus, WorkflowTaskResult};
use peri_workflow::runner::AgentExecutor;
use serde_json::{json, Value};
use serial_test::serial;

use super::*;
use crate::provider::LlmProvider;

#[path = "requests_recovery_test.rs"]
mod recovery_tests;
#[path = "requests/session_control_test.rs"]
mod session_control_tests;

#[path = "execution_work_test.rs"]
mod execution_work_tests;

#[path = "requests_legacy_test.rs"]
mod legacy_tests;

#[path = "requests_update_config_test.rs"]
mod update_config_tests;

#[path = "requests/acp_mcp_loop_test.rs"]
mod acp_mcp_loop_tests;

// ── Mock AcpTransport ─────────────────────────────────────────────────────────

/// 记录全部通知的 mock transport（`Mutex<Vec<(method, payload)>>`，Slice 6
/// 改造：原实现丢弃 `_params`，现记录供 available_commands_update 回调重发
/// 断言）。
#[derive(Default)]
struct MockTransport {
    notifications: std::sync::Mutex<Vec<(String, Value)>>,
}

impl MockTransport {
    fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::transport::AcpTransport for MockTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(
    id: &str,
    provider_type: &str,
    api_key: &str,
    model: &str,
) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: provider_type.to_string(),
        api_key: api_key.to_string(),
        // 将模型名填入 sonnet 别名（默认 alias）
        models: ProviderModels {
            sonnet: model.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造含单个 provider 的 PeriConfig（active_alias=sonnet），供 `LlmProvider::from_config` 使用。
fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider];
    peri_config
}

async fn make_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    // 生产同形入口：只注入门面（协议面、Controller、SessionManager 都只持它）。
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    build_server_config(peri_config, provider, tmp, session_resources).await
}

/// 门面 + 裸桥配对打开：夹具需要按 legacy/损坏事实逐条构造时用。
///
/// 二者出自**同一次打开**（同一库句柄）；裸句柄只用于建事实与
/// 直读断言，生产路径一律走注入的门面。
async fn make_server_config_with_bridge(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> (AcpServerConfig, Arc<SqliteThreadStore>) {
    let (bridge, facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let cfg = build_server_config(peri_config, provider, tmp, Arc::new(facade)).await;
    (cfg, Arc::new(bridge))
}

async fn build_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
    session_resources: Arc<dyn SessionResources>,
) -> AcpServerConfig {
    let session_manager = crate::session::SessionManager::new(
        session_resources.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        // 注入真实 TaskManager 工厂：cancel-bg-task 回归测试依赖 registry 簿记
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    AcpServerConfig {
        execution_admission_port: None,
        workspace_assembly: None,
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        mcp_apps_relay: None,
        acp_mcp: None,
        dynamic_mcp: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        agent_catalog: Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        workflow_middleware_factory: Arc::new(
            peri_middlewares::assembly::WorkflowAgentMiddlewareFactory,
        ),
        session_resources: session_resources.clone(),
        // 测试宿主：不注入部署关闭权（没有部署生命周期）。
        session_store_shutdown: None,
        controller: Arc::new(peri_controller::Controller::new(session_resources)),
        langfuse_session: None,
        langfuse_shutdown_owner: None,
        config_source: Arc::new(
            // 空 cwd（无工作区配置）+ 显式全局路径：persist_config 写回该路径
            crate::provider::ConfigSource::load_at(
                &tmp.path().join("empty-cwd"),
                tmp.path().join("test_config.json"),
            )
            .unwrap(),
        ),
        session_manager,
        stdio_command_filter: false,
    }
}

/// 夹具建一条**已绑定**会话：门面一次保存 binding/frozen。
async fn create_bound_fixture(cfg: &AcpServerConfig, cwd: &str, id: Option<&str>) -> String {
    let workspace = cfg
        .session_resources
        .resolve_workspace(Path::new(cwd))
        .await
        .unwrap();
    let thread_id = id.map(str::to_owned).unwrap_or_else(new_session_id);
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    cfg.session_resources
        .create_session(&bound_input(&thread_id, &workspace, encoded))
        .await
        .unwrap();
    thread_id
}

/// 新会话标识（与生产创建路径同源）。
fn new_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// 固定身份的已绑定会话输入（binding 由 workspace 事实构造）。
fn bound_input(
    thread_id: &str,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    frozen_encoded: String,
) -> NewSession {
    NewSession {
        thread_id: thread_id.to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: None,
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: peri_acp_types::thread::CancelPolicy::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: FrozenSnapshotBytes::new(frozen_encoded),
    }
}

async fn append_human_message(cfg: &AcpServerConfig, id: &str, text: &str) {
    cfg.session_resources
        .append_history(
            &id.to_owned(),
            &[PersistedPayload::Message(
                peri_acp_types::messages::BaseMessage::human(text),
            )],
        )
        .await
        .unwrap();
}

/// 门面读取 frozen 字节；`None` = legacy 尚未持久化快照。
async fn frozen_snapshot_bytes(cfg: &AcpServerConfig, id: &str) -> Option<String> {
    match cfg
        .session_resources
        .load_session_snapshot(&id.to_owned())
        .await
        .unwrap()
        .frozen
    {
        FrozenState::Present(bytes) => Some(bytes.into_string()),
        FrozenState::LegacyAbsent => None,
        FrozenState::Unsupported => {
            panic!("fixture frozen snapshot must be readable by this build")
        }
    }
}

/// 门面读取自有 payload（不含继承区）。
async fn own_payloads(cfg: &AcpServerConfig, id: &str) -> Vec<PersistedPayload> {
    cfg.session_resources
        .load_session_snapshot(&id.to_owned())
        .await
        .unwrap()
        .payloads
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// 注册一个含 user/ai 消息的 SessionState（字段以 mod.rs 定义为准）。
async fn register_session_with_history(
    sessions: &mut HashMap<String, SessionState>,
    cwd: &str,
    cfg: &AcpServerConfig,
) -> String {
    let history = vec![
        peri_acp_types::messages::BaseMessage::human("第一轮用户问题"),
        peri_acp_types::messages::BaseMessage::ai("第一轮回答"),
        peri_acp_types::messages::BaseMessage::human("第二轮用户问题"),
    ];
    let history_payloads: Vec<PersistedPayload> = history
        .iter()
        .cloned()
        .map(peri_acp_types::store::PersistedPayload::Message)
        .collect();
    let sid = "rewind-test-session".to_string();
    let workspace = cfg
        .session_resources
        .resolve_workspace(Path::new(cwd))
        .await
        .unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    cfg.session_resources
        .create_session(&bound_input(&sid, &workspace, encoded))
        .await
        .unwrap();
    cfg.session_resources
        .append_history(&sid, &history_payloads)
        .await
        .unwrap();
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: workspace.cwd.to_str().unwrap().to_owned(),
            environment: None,
            closing: false,
            history,
            history_payloads,
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            title: None,
            tags: Vec::new(),
        },
    );
    sid
}

// ── Phase 6 B3：plugin install/uninstall RPC 级投影断言（P2-2）──────────────

/// 测试期重定向 `$HOME`（插件入口的 `claude_dir` 经
/// `peri_middlewares::plugin::claude_home()` 计算：HOME 优先且要求绝对路径，
/// 两个平台都被本重定向覆盖；`refresh_plugin_command_entries` 经真实
/// `load_enabled_plugins` 重载）；Drop 时还原。进程级 env 态 →
/// 本组用例全部 `#[serial]`（与 store_test 同组互斥）。Windows 一并设置
/// `USERPROFILE`（与 `HOME` 同源；`dirs_next::home_dir()` 在该平台走 Profile
/// known-folder，不读环境变量，故不构成隔离手段）。
struct HomeDirGuard {
    home: Option<std::ffi::OsString>,
    #[cfg(windows)]
    userprofile: Option<std::ffi::OsString>,
}

impl HomeDirGuard {
    fn set(path: &Path) -> Self {
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        #[cfg(windows)]
        {
            let prev = std::env::var_os("USERPROFILE");
            std::env::set_var("USERPROFILE", path);
            Self {
                home,
                userprofile: prev,
            }
        }
        #[cfg(not(windows))]
        Self { home }
    }
}

fn restore_env_var(slot: &mut Option<std::ffi::OsString>, name: &str) {
    match slot.take() {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

impl Drop for HomeDirGuard {
    fn drop(&mut self) {
        restore_env_var(&mut self.home, "HOME");
        #[cfg(windows)]
        restore_env_var(&mut self.userprofile, "USERPROFILE");
    }
}

/// 可编程 `PluginManagerPort` mock：install / uninstall 结果注入，其余方法
/// 空实现（install/uninstall 分支仅消费 install/uninstall + snapshot +
/// cache_dir；`unstable_event` caps 默认关闭，push_plugin_* 不发通知）。
struct MockPluginManager {
    cache_dir: PathBuf,
    install_result: std::sync::Mutex<Result<InstalledPlugin, String>>,
    uninstall_result: std::sync::Mutex<Result<(), String>>,
}

impl MockPluginManager {
    fn install_ok(id: &str) -> Self {
        Self {
            cache_dir: PathBuf::from("/tmp/mock-cache"),
            install_result: std::sync::Mutex::new(Ok(InstalledPlugin {
                id: id.to_string(),
                name: id.to_string(),
                version: "1.0.0".into(),
                marketplace: "test-mkt".into(),
                install_path: PathBuf::from("/tmp/mock-install"),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            })),
            uninstall_result: std::sync::Mutex::new(Ok(())),
        }
    }
}

#[async_trait]
impl PluginManagerPort for MockPluginManager {
    async fn install(
        &self,
        _name: &str,
        _marketplace: &str,
        _scope: InstallScope,
        _cache_dir: &Path,
        _claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        self.install_result.lock().unwrap().clone()
    }

    async fn uninstall(&self, _plugin_id: &str, _claude_dir: &Path) -> Result<(), String> {
        self.uninstall_result.lock().unwrap().clone()
    }

    fn set_enabled(
        &self,
        _plugin_id: &str,
        _scope: InstallScope,
        _claude_dir: &Path,
        _enable: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone()
    }

    async fn update(
        &self,
        _plugin_id: &str,
        _cache_dir: &Path,
        _claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        Err("mock: unused".into())
    }

    async fn refresh_marketplace(&self, _name: &str) -> Result<usize, String> {
        Err("mock: unused".into())
    }

    async fn cleanup(&self, _claude_dir: &Path) -> Result<usize, String> {
        Err("mock: unused".into())
    }

    async fn marketplace_add(&self, _source: &str) -> Result<String, String> {
        Err("mock: unused".into())
    }

    async fn marketplace_remove(&self, _name: &str) -> Result<(), String> {
        Err("mock: unused".into())
    }

    async fn marketplace_update(&self, _name: &str) -> Result<String, String> {
        Err("mock: unused".into())
    }

    fn marketplace_snapshot(&self) -> Value {
        json!({})
    }

    fn snapshot(&self, _claude_dir: &Path) -> Vec<PluginSnapshotEntry> {
        vec![]
    }

    // W3 端口补全：命令重载 / 路由投影 / 目录定位按真实加载器委托——与替换前
    // ACP 直调 middlewares 静态函数的行为逐字节一致（磁盘夹具依赖这一路径）。
    fn claude_home(&self) -> PathBuf {
        peri_middlewares::plugin::claude_home()
    }

    fn enabled_plugin_commands(
        &self,
        claude_dir: &Path,
        cwd: Option<&Path>,
    ) -> Result<Vec<peri_acp_types::plugin::CommandEntry>, String> {
        peri_middlewares::plugin::load_enabled_plugins(claude_dir, cwd)
            .map(|plugins| plugins.into_iter().flat_map(|p| p.commands).collect())
            .map_err(|error| error.to_string())
    }

    fn plugin_route_entries(
        &self,
        entries: &[peri_acp_types::plugin::CommandEntry],
    ) -> Vec<peri_acp_types::command::command_route::RouteEntry> {
        peri_middlewares::plugin::plugin_route_entries(entries)
    }

    fn find_marketplace_json(&self, dir: &Path) -> Option<PathBuf> {
        peri_middlewares::plugin::marketplace::find_marketplace_json(dir)
    }
}

/// 在 `{home}/.claude` 布置一个启用中的插件 `ecc`（含命令
/// `commands/deploy.md`），供 `refresh_plugin_command_entries` 重载出
/// `plugin:ecc:deploy`（与 peri-middlewares loader_test 的磁盘形态同构）。
#[path = "user_input_test.rs"]
mod user_input_tests;

#[path = "requests/plugin_search_test.rs"]
mod plugin_search_tests;

#[path = "requests_config_cases_test.rs"]
mod config_cases;

#[path = "requests_rewind_cases_test.rs"]
mod rewind_cases;

#[path = "requests_workflow_cases_test.rs"]
mod workflow_cases;

#[path = "requests_lifecycle_cases_test.rs"]
mod lifecycle_cases;

#[path = "requests_mcp_cases_test.rs"]
mod mcp_cases;

#[path = "requests_frozen_cases_test.rs"]
mod frozen_cases;

#[path = "requests_plugins_cases_test.rs"]
mod plugins_cases;

#[path = "requests_workspace_cases_test.rs"]
mod workspace_cases;

#[path = "requests_meta_resources_test.rs"]
mod meta_resources;

#[path = "requests_skill_resources_test.rs"]
mod skill_resources;

#[path = "requests_cron_test.rs"]
mod cron_tests;

fn seed_plugin_ecc(home: &Path) {
    let claude_dir = home.join(".claude");
    let plugin_dir = claude_dir.join("plugins").join("ecc");
    // 命令文件相对插件根目录（extract_commands: base_dir.join(path)）
    std::fs::create_dir_all(plugin_dir.join("commands")).unwrap();
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"ecc","version":"1.0.0","commands":[{"path":"commands/deploy.md"}]}"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("commands").join("deploy.md"),
        "---\ndescription: Deploy to prod\n---\nBody",
    )
    .unwrap();
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json =
        serde_json::to_string(&peri_middlewares::plugin::types::InstalledPlugins {
            version: 2,
            plugins: vec![InstalledPlugin {
                id: "ecc@test-mkt".into(),
                name: "ecc".into(),
                version: "1.0.0".into(),
                marketplace: "test-mkt".into(),
                install_path: plugin_dir,
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            }],
        })
        .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["ecc@test-mkt"]}"#,
    )
    .unwrap();
}

struct MockWorkflowExecutor;

#[async_trait]
impl AgentExecutor for MockWorkflowExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        AgentRunResult::Ok {
            output: serde_json::json!("mock"),
            usage: Usage { output_tokens: 0 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

/// 构造带 workflow_middleware 的 SessionState，返回 middleware 引用（供注册 run 用）。
async fn register_session_with_workflow(
    sessions: &mut HashMap<String, SessionState>,
    sid: &str,
    cwd: &str,
    cfg: &AcpServerConfig,
) -> Arc<WorkflowMiddleware> {
    // 会话尚未创建（NotFound）或没有本机绑定：本夹具负责建一条已绑定会话。
    let bound = matches!(
        cfg.session_resources
            .load_session_binding(&sid.to_owned())
            .await,
        Ok(BindingState::Bound(_))
    );
    if !bound {
        create_bound_fixture(cfg, cwd, Some(sid)).await;
    }
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockWorkflowExecutor);
    let (notification_tx, _) = tokio::sync::broadcast::channel::<WorkflowTaskResult>(32);
    let mw = Arc::new(WorkflowMiddleware::new(
        executor,
        cwd,
        notification_tx,
        None,
    ));
    sessions.insert(
        sid.to_string(),
        SessionState {
            session_id: sid.to_string(),
            thread_id: sid.to_owned(),
            cwd: std::fs::canonicalize(cwd)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            environment: None,
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: Some(Arc::clone(&mw) as Arc<dyn WorkflowMiddlewarePort>),
            title: None,
            tags: Vec::new(),
        },
    );
    mw
}
