// ── State ─────────────────────────────────────────────────────────────────────
mod global_ui_state;
pub mod service_registry;
pub use global_ui_state::GlobalUiState;
pub use service_registry::ServiceRegistry;

// ── Provider ──────────────────────────────────────────────────────────────────
pub mod agent;
pub use agent::LlmProvider;

// ── UI Interaction ────────────────────────────────────────────────────────────
pub mod panel_types;
pub use panel_types::PanelKind;

pub mod setup_wizard;

mod cron_state;
pub use cron_state::CronState;

// ── Services ───────────────────────────────────────────────────────────────────
mod provider;

use crate::acp_client::AcpTuiClient;
use crate::config::PeriConfig;
use std::path::PathBuf;

// ─── App ──────────────────────────────────────────────────────────────────────

pub struct App {
    /// 全局服务/状态聚合（跨 session 共享）
    pub services: ServiceRegistry,
    /// 跨 session 全局 UI 临时状态
    pub global_ui: GlobalUiState,
    /// 应用焦点状态（true=聚焦，false=失焦）
    pub focused: bool,
    /// 配置源（读写路径决策的唯一事实源；TUI 面板保存与 ACP persist_config
    /// 共享同一 `Arc`，见 [`crate::config::ConfigSource`]）
    pub config_source: std::sync::Arc<crate::config::ConfigSource>,
    /// ACP client — communicates with the ACP server via in-memory transport.
    /// Initialized after App construction in run_app(); None until `set_acp_client` is called.
    pub acp_client: Option<AcpTuiClient>,
}

impl App {
    /// `db_path`：显式指定 SQLite 会话数据库路径；`None` 保持默认路径
    /// + fallback 临时目录行为（`Resources::open_with`）。
    pub async fn new(db_path: Option<PathBuf>) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // 工具卡片头行路径精简用（进程生命周期内不变）
        crate::truncate::set_display_cwd(cwd.clone());

        // 配置源：启动时一次性探测「全局 + 工作区」布局（P0 分层语义——
        // 加载与保存共享同一路径决策）；解析失败按空配置继续（容错，与
        // 迁移前 `load().ok()` 行为一致），回退环境变量。
        let config_source = std::sync::Arc::new(crate::config::ConfigSource::load_lenient());
        let peri_config = Some(config_source.loaded_merged());

        let lc = crate::i18n::LcRegistry::new(
            peri_config
                .as_ref()
                .and_then(|c| c.config.language.as_deref()),
        );

        let provider_from_config = peri_config
            .as_ref()
            .and_then(agent::LlmProvider::from_config);
        let provider_name = match provider_from_config.or_else(agent::LlmProvider::from_env) {
            Some(p) => {
                let name = p.display_name().to_string();
                let model = p.model_name().to_string();
                let _msg = lc.tr_args(
                    "app-provider-ready",
                    &[
                        ("name".into(), name.clone().into()),
                        ("model".into(), model.into()),
                    ],
                );
                name
            }
            None => lc.tr("app-not-configured"),
        };

        // 初始化 thread 存储（经 Resources 门面；失败时 fallback 到临时目录的逻辑在门面内。
        // db_path 显式指定时打开失败直接上抛——TUI 路径由 run_tui 决定 exit 码）
        let resources = peri_resources::Resources::open_with(db_path)
            .await
            .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;
        let thread_store: std::sync::Arc<dyn crate::thread::ThreadStore> = resources.thread_store();

        // 初始化 cron state + spawn tick task
        let (cron_state, scheduler_arc) = CronState::new();
        CronState::spawn_tick_task(scheduler_arc);

        let permission_mode = peri_acp_types::permission::SharedPermissionMode::new(
            peri_acp_types::permission::PermissionMode::Bypass,
        );
        let services = ServiceRegistry {
            peri_config: std::sync::Arc::new(parking_lot::RwLock::new(
                peri_config.clone().unwrap_or_default(),
            )),
            cwd: cwd.clone(),
            provider_name: provider_name.clone(),
            permission_mode: permission_mode.clone(),
            thread_store: thread_store.clone(),
            mcp_pool: None,
            mcp_init_rx: None,
            cron: cron_state,
            plugin_data: None,
            resource_monitor: parking_lot::Mutex::new(
                service_registry::ProcessResourceMonitor::new(),
            ),
        };

        Ok(Self {
            services,
            global_ui: GlobalUiState::new(),
            focused: true,
            config_source,
            acp_client: None,
        })
    }

    /// 后台初始化 MCP 连接池（不阻塞 UI），在 run_app 中 App::new() 之后调用
    pub fn spawn_mcp_init(&mut self) {
        // MCP 资源句柄直读（C 类豁免至 M-TUI；「面板数据全部经 ACP」需
        // mcp/list 命令面，见批 3 tui-deps 未做项）
        let pool = std::sync::Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
        self.services.mcp_pool = Some(pool.clone());
        // 面板直读句柄：OAuth 授权完成后（kit 层 OauthCompleted 事件）据此
        // reconnect，从共享凭证文件恢复连接。
        let _ = crate::kit::atoms::MCP_PANEL_POOL.set(pool.clone());

        let (init_tx, init_rx) =
            tokio::sync::watch::channel(peri_middlewares::mcp::McpInitStatus::Pending);
        self.services.mcp_init_rx = Some(init_rx);

        let cwd = self.services.cwd.clone();
        let claude_home = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".claude");

        tokio::spawn(async move {
            peri_middlewares::mcp::McpClientPool::run_initialize(
                pool,
                std::path::Path::new(&cwd),
                &claude_home,
                init_tx,
                None,
                None,
            )
            .await;
        });
    }

    /// 保存配置：优先写入 override 路径（测试用），否则写回当前生效层
    /// （路径决策在 ConfigSource 加载时确定，见 [`crate::config::save_effective`]）
    pub fn save_config(
        cfg: &PeriConfig,
        override_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        match override_path {
            Some(path) => crate::config::save_to(cfg, path),
            None => crate::config::save_effective(cfg),
        }
    }

    /// Setup 向导保存后刷新内存中的 Provider 状态。
    ///
    /// 配置写入共享的 `Arc<RwLock<PeriConfig>>`，ACP Server 持有同一 `Arc`，
    /// 因此无需再调用 `sync_acp_config`。
    pub fn refresh_after_setup(&mut self, cfg: crate::config::PeriConfig) {
        *self.services.peri_config.write() = cfg;
        let cfg_ref = self.services.peri_config.read();
        if let Some(p) = agent::LlmProvider::from_config(&cfg_ref) {
            self.services.provider_name = p.display_name().to_string();
        }
    }

    pub fn get_compact_config(&self) -> peri_acp_types::compact::CompactConfig {
        let mut config = self
            .services
            .peri_config
            .read()
            .config
            .compact
            .clone()
            .unwrap_or_default();
        config.apply_env_overrides();
        config
    }
}
