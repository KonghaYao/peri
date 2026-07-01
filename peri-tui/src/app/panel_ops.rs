#[cfg(any(test, feature = "headless"))]
use super::*;

// panel_ops.rs — test helpers for App.
//
// Panel operation functions have been split into per-panel submodules:
//   panel_model, panel_login, panel_config, panel_status,
//   panel_memory, panel_plugin, panel_agent, panel_hooks.
// Each submodule contributes inherent impl App blocks directly.

// ─── 测试辅助方法（仅在 cfg(any(test, feature = "headless")) 下编译）──────────

#[cfg(any(test, feature = "headless"))]
impl App {
    /// 向事件队列注入 AgentEvent（测试用）
    pub fn push_agent_event(&mut self, event: AgentEvent) {
        self.session_mgr
            .current_mut()
            .agent
            .agent_event_queue
            .push(event);
    }

    /// P5: No-op — sync rendering replaces pipeline rebuild
    pub fn flush_rebuild(&mut self) {}

    /// 批量处理队列中所有待处理事件，复用 handle_agent_event 逻辑
    pub fn process_pending_events(&mut self) {
        let events: Vec<AgentEvent> =
            std::mem::take(&mut self.session_mgr.current_mut().agent.agent_event_queue);
        for event in events {
            // Phase 2.6 step 7c: test-only path — pass empty v2 view slice.
            // Production path goes through main_loop::handle_acp_event which
            // captures state.view_models() snapshot. Tests that exercise the
            // interrupt path should use the main_loop integration tests instead.
            let empty_view: [peri_acp_types::view_model::ViewModel; 0] = [];
            let (_updated, should_break, should_return) =
                self.handle_agent_event(event, &empty_view);
            if should_return || should_break {
                break;
            }
        }
    }

    /// 构造 Headless 测试用 App，使用 ratatui TestBackend 替代真实终端
    pub async fn new_headless(
        width: u16,
        height: u16,
    ) -> (App, crate::ui::headless::HeadlessHandle) {
        use ratatui::{backend::TestBackend, Terminal};

        use crate::thread::SqliteThreadStore;

        let backend = TestBackend::new(width, height);
        let terminal = Terminal::new(backend).expect("TestBackend should never fail");

        // P5: No render thread — sync rendering

        // 使用唯一临时 SQLite 存储，避免测试并发时文件锁冲突
        let db_name = format!("zen-threads-test-{}.db", uuid::Uuid::now_v7());
        let thread_store: Arc<dyn ThreadStore> = Arc::new(
            SqliteThreadStore::new(std::env::temp_dir().join(db_name))
                .await
                .expect("无法创建测试用 SQLite 数据库"),
        );

        // 将配置路径重定向到临时目录，防止测试污染全局 ~/.peri/settings.json
        let test_config_path = std::env::temp_dir().join(format!(
            "zen-config-test-{}/settings.json",
            uuid::Uuid::now_v7()
        ));

        let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::channel(128);

        let lc = crate::i18n::LcRegistry::default();
        let commands =
            super::CommandSystem::new(crate::command::default_registry(), Vec::new(), &lc);

        let session = super::ChatSession {
            ui: super::UiState::new(super::build_textarea(false), "/tmp", false),
            messages: super::MessageState::new(),
            commands,
            metadata: super::SessionMetadata::new(),
            agent: super::AgentComm::default(),
            langfuse: super::LangfuseState::default(),
            current_thread_id: None,
            todo_items: Vec::new(),
            background_agents: Vec::new(),
            focused_instance_id: None,
            spinner_state: peri_widgets::SpinnerState::new(peri_widgets::SpinnerMode::Idle),
            subagent_status: super::SubAgentStatusMap::new(),
        };

        let app = App {
            session_mgr: super::SessionManager::new(session),
            services: super::ServiceRegistry {
                peri_config: std::sync::Arc::new(parking_lot::RwLock::new(
                    crate::config::PeriConfig::default(),
                )),
                cwd: "/tmp".to_string(),
                provider_name: "test".to_string(),
                model_name: "test-model".to_string(),
                permission_mode: peri_middlewares::prelude::SharedPermissionMode::new(
                    peri_middlewares::prelude::PermissionMode::Bypass,
                ),
                thread_store,
                mcp_pool: None,
                mcp_init_rx: None,
                cron: super::CronState::default(),
                plugin_data: None,
                bg_event_tx,
                bg_event_rx: Some(bg_event_rx),
                config_path_override: Some(test_config_path),
                claude_settings_override: Some(std::env::temp_dir().join(format!(
                    "claude-settings-test-{}.json",
                    uuid::Uuid::now_v7()
                ))),
                resource_monitor: parking_lot::Mutex::new(
                    super::service_registry::ProcessResourceMonitor::new(),
                ),
                lc: crate::i18n::LcRegistry::default(),
                channel_state: None,
                panic_notify_rx: None,
                acp_session_manager: None,
            },
            global_ui: super::GlobalUiState::new(),
            workflow_poll_rx: None,
            workflow_poll_kill: None,
            workflow_polling_active: false,
            focused: true,
            acp_client: None,
        };

        let handle = crate::ui::headless::HeadlessHandle { terminal };

        (app, handle)
    }
}
