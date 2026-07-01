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
pub use panel_types::{MutexGroup, PanelKind, PanelScope};

pub mod setup_wizard;
pub use setup_wizard::SetupWizardPanel;

mod cron_state;
pub use cron_state::CronState;

mod field_textarea;
pub use field_textarea::FieldTextarea;

// ── Services ───────────────────────────────────────────────────────────────────
mod provider;

use crate::acp_client::AcpTuiClient;
use crate::config::PeriConfig;

// ─── App ──────────────────────────────────────────────────────────────────────

pub struct App {
    /// 全局服务/状态聚合（跨 session 共享）
    pub services: ServiceRegistry,
    /// 跨 session 全局 UI 临时状态
    pub global_ui: GlobalUiState,
    /// 应用焦点状态（true=聚焦，false=失焦）
    pub focused: bool,
    /// ACP client — communicates with the ACP server via in-memory transport.
    /// Initialized after App construction in run_app(); None until `set_acp_client` is called.
    pub acp_client: Option<AcpTuiClient>,
}

impl App {
    pub async fn new() -> Self {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // 优先从 ~/.peri/settings.json 加载配置，失败时 fallback 到环境变量
        let peri_config = crate::config::load().ok();

        let lc = crate::i18n::LcRegistry::new(
            peri_config
                .as_ref()
                .and_then(|c| c.config.language.as_deref()),
        );

        let provider_from_config = peri_config
            .as_ref()
            .and_then(agent::LlmProvider::from_config);
        let (provider_name, model_name, _status_msg) =
            match provider_from_config.or_else(agent::LlmProvider::from_env) {
                Some(p) => {
                    let name = p.display_name().to_string();
                    let model = p.model_name().to_string();
                    let msg = lc.tr_args(
                        "app-provider-ready",
                        &[
                            ("name".into(), name.clone().into()),
                            ("model".into(), model.clone().into()),
                        ],
                    );
                    (name, model, msg)
                }
                None => (
                    lc.tr("app-not-configured"),
                    lc.tr("app-empty"),
                    lc.tr("app-no-api-key-warning"),
                ),
            };

        // 初始化 thread 存储（失败时 fallback 到临时目录）
        let thread_store: std::sync::Arc<dyn crate::thread::ThreadStore> =
            match crate::thread::SqliteThreadStore::default_path().await {
                Ok(store) => std::sync::Arc::new(store),
                Err(_) => std::sync::Arc::new(
                    crate::thread::SqliteThreadStore::new(
                        std::env::temp_dir().join("zen-threads.db"),
                    )
                    .await
                    .expect("无法创建临时 SQLite 数据库"),
                ),
            };

        // 初始化 cron state + spawn tick task
        let (cron_state, scheduler_arc) = CronState::new();
        CronState::spawn_tick_task(scheduler_arc);

        let permission_mode = peri_middlewares::prelude::SharedPermissionMode::new(
            peri_middlewares::prelude::PermissionMode::Bypass,
        );
        let services = ServiceRegistry {
            peri_config: std::sync::Arc::new(parking_lot::RwLock::new(
                peri_config.clone().unwrap_or_default(),
            )),
            cwd: cwd.clone(),
            provider_name: provider_name.clone(),
            model_name: model_name.clone(),
            permission_mode: permission_mode.clone(),
            thread_store: thread_store.clone(),
            mcp_pool: None,
            mcp_init_rx: None,
            cron: cron_state,
            plugin_data: None,
            config_path_override: None,
            claude_settings_override: None,
            resource_monitor: parking_lot::Mutex::new(
                service_registry::ProcessResourceMonitor::new(),
            ),
            lc,
            panic_notify_rx: None,
            acp_session_manager: None,
        };

        Self {
            services,
            global_ui: GlobalUiState::new(),
            focused: true,
            acp_client: None,
        }
    }

    /// 后台初始化 MCP 连接池（不阻塞 UI），在 run_app 中 App::new() 之后调用
    pub fn spawn_mcp_init(&mut self) {
        use peri_middlewares::mcp::{McpClientPool, McpInitStatus};

        let pool = std::sync::Arc::new(McpClientPool::new_pending());
        self.services.mcp_pool = Some(pool.clone());

        let (init_tx, init_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        self.services.mcp_init_rx = Some(init_rx);

        let cwd = self.services.cwd.clone();
        let claude_home = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".claude");

        tokio::spawn(async move {
            McpClientPool::run_initialize(
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

    /// 保存配置：优先写入 override 路径（测试用），否则写入全局路径
    pub fn save_config(
        cfg: &PeriConfig,
        override_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        match override_path {
            Some(path) => crate::config::save_to(cfg, path),
            None => crate::config::save(cfg),
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
            self.services.model_name = p.model_name().to_string();
        }
    }

    pub fn get_compact_config(&self) -> peri_agent::agent::CompactConfig {
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

    /// 打开 setup 向导（全屏覆盖）
    pub fn open_setup_wizard(&mut self) {
        self.global_ui.setup_wizard =
            Some(crate::app::setup_wizard::SetupWizardPanel::new_from_command());
    }
}
