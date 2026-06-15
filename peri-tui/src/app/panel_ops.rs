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

    /// 强制从 pipeline 规范状态重建 view_messages 并发送 RenderEvent。
    /// 用于 headless 测试：确保流式缓冲区内容（throttle 未触发的 chunk）也被渲染。
    pub fn flush_rebuild(&mut self) {
        let prefix_len = self.session_mgr.current_mut().messages.round_start_vm_idx;
        let action = self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .build_rebuild_all(prefix_len);
        self.apply_pipeline_action(action);
    }

    /// 批量处理队列中所有待处理事件，复用 handle_agent_event 逻辑
    pub fn process_pending_events(&mut self) {
        let events: Vec<AgentEvent> =
            std::mem::take(&mut self.session_mgr.current_mut().agent.agent_event_queue);
        for event in events {
            let (_updated, should_break, should_return) = self.handle_agent_event(event);
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

        // 启动渲染线程
        let (render_tx, render_cache, render_notify) =
            crate::ui::render_thread::spawn_render_thread(width);

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
            messages: super::MessageState::new(
                "/tmp".to_string(),
                render_tx.clone(),
                std::sync::Arc::clone(&render_cache),
                std::sync::Arc::clone(&render_notify),
            ),
            session_panels: super::panel_manager::PanelManager::new(),
            commands,
            metadata: super::SessionMetadata::new(),
            agent: super::AgentComm::default(),
            langfuse: super::LangfuseState::default(),
            current_thread_id: None,
            todo_items: Vec::new(),
            background_agents: Vec::new(),
            focused_instance_id: None,
            spinner_state: peri_widgets::SpinnerState::new(peri_widgets::SpinnerMode::Idle),
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
            },
            global_panels: PanelManager::new(),
            global_ui: super::GlobalUiState::new(),
            focused: true,
            acp_client: None,
        };

        let handle = crate::ui::headless::HeadlessHandle {
            terminal,
            render_notify,
        };

        (app, handle)
    }
}
