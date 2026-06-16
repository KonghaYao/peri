// ── Panel Modules ────────────────────────────────────────────────────────────
pub mod agent_panel;
pub mod betas_panel;
pub mod config_panel;
pub mod hooks_panel;
pub mod login_panel;
pub mod mcp_panel;
pub mod memory_panel;
pub mod model_panel;
pub mod panel_component;
pub mod panel_list;
pub mod panel_manager;
pub mod panel_plugin;
pub mod plugin_panel;
pub mod setup_wizard;
pub mod status_panel;
pub mod tasks_panel;

// Panel private modules
mod panel_agent;
mod panel_betas;
mod panel_config;
mod panel_hooks;
mod panel_login;
mod panel_memory;
mod panel_model;
mod panel_ops;
mod panel_status;

// ── State Management ─────────────────────────────────────────────────────────
mod global_ui_state;
mod service_registry;
pub use global_ui_state::GlobalUiState;
pub use service_registry::ServiceRegistry;

mod session_manager;
pub use session_manager::SessionManager;

mod ui_state;
pub use ui_state::UiState;

pub(crate) mod at_mention;
pub use at_mention::AtMentionState;

mod message_state;
pub use message_state::MessageState;

// ── Agent Communication ──────────────────────────────────────────────────────
mod agent_comm;
mod agent_compact;
mod agent_events_bg;
mod agent_events_oauth;
mod agent_events_plugin;
mod agent_ops;
mod agent_ops_interaction;
mod agent_render;
mod agent_submit;
mod ask_user_ops;
mod ask_user_prompt;
pub use ask_user_prompt::AskUserBatchPrompt;
mod cron_ops;
mod cron_state;
mod hint_ops;
pub use hint_ops::SlashHintState;
mod history_ops;
mod history_persistence;
mod hitl_ops;
mod hitl_prompt;
pub use hitl_prompt::{HitlBatchPrompt, PendingAttachment};
mod rewind_prompt;
pub use rewind_prompt::{FileChangeInfo, RewindItem, RewindMode, RewindPrompt};

// ── System Infrastructure ────────────────────────────────────────────────────
mod chat_session;
mod command_system;
mod ime;
mod session_metadata;
pub use chat_session::ChatSession;
#[cfg(test)]
pub(crate) use chat_session::RunningBgAgent;
pub use command_system::CommandSystem;
pub use ime::textarea_cursor_pos;
pub use session_metadata::SessionMetadata;

mod langfuse_state;
mod oauth_prompt;
pub use oauth_prompt::OAuthPrompt;
mod thread_ops;

// ── Other Modules ─────────────────────────────────────────────────────────────
pub mod agent;
pub mod events;
pub mod message_pipeline;
mod provider;
pub mod text_selection;
pub mod tool_display;

// Re-exports
pub use events::AgentEvent;

/// 统一交互弹窗枚举：同一时刻只允许一种弹窗激活
mod interaction;
pub use interaction::InteractionPrompt;

mod edit_utils;
pub use edit_utils::{build_textarea, ensure_cursor_visible};

mod field_textarea;
use std::sync::Arc;

pub use agent::LlmProvider;
// Re-export sub-structs
pub use agent_comm::{AgentComm, RetryStatus};
pub use agent_panel::AgentPanel;
pub use cron_state::{CronPanel, CronState};
pub use field_textarea::FieldTextarea;
pub use hooks_panel::HooksPanel;
pub use langfuse_state::LangfuseState;
pub use mcp_panel::{DetailAction, McpPanel, McpPanelView};
pub use model_panel::ModelPanel;
pub use panel_component::PanelComponent;
pub use panel_manager::{
    EventResult, MutexGroup, PanelContext, PanelKind, PanelManager, PanelScope, PanelState,
};
use peri_agent::messages::BaseMessage;
use peri_middlewares::prelude::HitlDecision;
pub use setup_wizard::SetupWizardPanel;
pub use tasks_panel::TasksPanel;

use crate::acp_client::{AcpNotification, AcpTuiClient};
// Re-export MessageViewModel from ui::message_view
use crate::command::agents::AgentItem;
pub use crate::ui::message_view::{
    aggregate_tail_tool_groups, aggregate_tool_groups, ContentBlockView, MessageViewModel,
    ToolCategory,
};
use crate::ui::render_thread::RenderEvent;
use crate::{
    config::PeriConfig,
    thread::{SqliteThreadStore, ThreadBrowser, ThreadId, ThreadStore},
};

// ─── App ──────────────────────────────────────────────────────────────────────

pub struct App {
    /// 会话管理器（单个 ChatSession）
    pub session_mgr: SessionManager,
    /// 全局服务/状态聚合（跨 session 共享）
    pub services: ServiceRegistry,
    /// 跨 session 全局 UI 临时状态
    pub global_ui: GlobalUiState,
    pub global_panels: panel_manager::PanelManager,
    /// 应用焦点状态（true=聚焦，false=失焦）
    pub focused: bool,
    /// ACP client — communicates with the ACP server via in-memory transport.
    /// Initialized after App construction in run_app(); None until `set_acp_client` is called.
    /// Added in Step 6-a; fully integrated in Steps 6-c..6-h.
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
        let thread_store: Arc<dyn ThreadStore> = match SqliteThreadStore::default_path().await {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(
                SqliteThreadStore::new(std::env::temp_dir().join("zen-threads.db"))
                    .await
                    .expect("无法创建临时 SQLite 数据库"),
            ),
        };

        // 预计算命令帮助列表
        let command_registry = crate::command::default_registry();
        let skills = {
            let mut dirs = Vec::new();
            if let Some(home) = dirs_next::home_dir() {
                dirs.push(home.join(".claude").join("skills"));
            }
            if let Some(global_dir) = peri_middlewares::skills::load_global_skills_dir() {
                dirs.push(global_dir);
            }
            if let Ok(cwd) = std::env::current_dir() {
                dirs.push(cwd.join(".claude").join("skills"));
            }
            peri_middlewares::skills::list_skills(&dirs)
        };

        // 初始化 cron state + spawn tick task
        let (cron_state, scheduler_arc) = CronState::new();
        CronState::spawn_tick_task(scheduler_arc);

        let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::channel(128);

        let diff_enabled = peri_config
            .as_ref()
            .map(|c| c.config.diff_enabled)
            .unwrap_or(false);
        let streaming_mode = peri_config
            .as_ref()
            .and_then(|c| c.config.streaming_mode.clone());

        let initial_session = ChatSession::new(
            cwd.clone(),
            command_registry,
            skills,
            &lc,
            diff_enabled,
            streaming_mode,
        );

        let session_mgr = SessionManager::new(initial_session);

        let permission_mode = peri_middlewares::prelude::SharedPermissionMode::new(
            peri_middlewares::prelude::PermissionMode::Bypass,
        );
        let channel_state = peri_agent::interaction::ChannelState::new();
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
            bg_event_tx: bg_event_tx.clone(),
            bg_event_rx: Some(bg_event_rx),
            config_path_override: None,
            claude_settings_override: None,
            resource_monitor: parking_lot::Mutex::new(
                service_registry::ProcessResourceMonitor::new(),
            ),
            lc,
            channel_state: Some(channel_state.clone()),
            panic_notify_rx: None,
        };

        Self {
            session_mgr,
            services,
            global_ui: GlobalUiState::new(),
            global_panels: panel_manager::PanelManager::new(),
            focused: true,
            acp_client: None,
        }
    }

    // ─── Session 访问器 ─────────────────────────────────────────────────────

    /// 获取当前激活 session 的不可变引用
    pub fn active(&self) -> &ChatSession {
        self.session_mgr.current()
    }

    /// 获取当前激活 session 的可变引用
    pub fn active_mut(&mut self) -> &mut ChatSession {
        self.session_mgr.current_mut()
    }

    /// 创建新 session 并替换当前 session（用于 /clear）
    pub fn new_session(&mut self) {
        // 取消旧 session 的 agent
        if let Some(token) = &self.session_mgr.current_mut().agent.cancel_token {
            token.cancel();
        }
        let mut command_registry = crate::command::default_registry();
        let mut skills = {
            let mut dirs = Vec::new();
            if let Some(home) = dirs_next::home_dir() {
                dirs.push(home.join(".claude").join("skills"));
            }
            if let Some(global_dir) = peri_middlewares::skills::load_global_skills_dir() {
                dirs.push(global_dir);
            }
            if let Ok(cwd) = std::env::current_dir() {
                dirs.push(cwd.join(".claude").join("skills"));
            }
            peri_middlewares::skills::list_skills(&dirs)
        };
        // 追加插件 skills（去重）
        if let Some(pd) = &self.services.plugin_data {
            let plugin_skills = peri_middlewares::skills::list_skills(&pd.all_skill_dirs);
            let existing_names: std::collections::HashSet<String> =
                skills.iter().map(|s| s.name.clone()).collect();
            for skill in plugin_skills {
                if !existing_names.contains(&skill.name) {
                    skills.push(skill);
                }
            }
            command_registry.register_plugin_commands(pd.all_commands.clone());
        }
        let diff_visible = self.session_mgr.current_mut().ui.diff_visible;
        let streaming_mode = self
            .services
            .peri_config
            .read()
            .config
            .streaming_mode
            .clone();
        let session = ChatSession::new(
            self.services.cwd.clone(),
            command_registry,
            skills,
            &self.services.lc,
            diff_visible,
            streaming_mode,
        );
        self.session_mgr.replace(session);
    }

    /// 后台初始化 MCP 连接池（不阻塞 UI），在 run_app 中 App::new() 之后调用
    pub fn spawn_mcp_init(&mut self) {
        use peri_middlewares::mcp::{McpClientPool, McpInitStatus};

        let pool = Arc::new(McpClientPool::new_pending());
        self.services.mcp_pool = Some(pool.clone());

        let (init_tx, init_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        self.services.mcp_init_rx = Some(init_rx);

        let cwd = self.services.cwd.clone();
        let tx = self.services.bg_event_tx.clone();
        let oauth_cb: Box<dyn Fn(peri_middlewares::mcp::OAuthFlowEvent) + Send + Sync> =
            Box::new(move |ev| {
                use peri_middlewares::mcp::OAuthFlowEvent;
                if let OAuthFlowEvent::AuthorizationNeeded {
                    server_name,
                    authorization_url,
                    callback_tx,
                } = ev
                {
                    let _ = tx.try_send(events::AgentEvent::OAuthAuthorizationNeeded {
                        server_name,
                        authorization_url,
                        callback_tx,
                    });
                }
            });

        let claude_home = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".claude");

        tokio::spawn(async move {
            McpClientPool::run_initialize(
                pool,
                std::path::Path::new(&cwd),
                &claude_home,
                init_tx,
                Some(oauth_cb),
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

    // ─── 转发访问器（通过 active session 路由）──────────────────────────────

    /// 中断正在运行的 Agent（Ctrl+C during loading）
    pub fn interrupt(&mut self) {
        // Try ACP cancel first (agent runs in ACP server)
        // Spawn cancel async without blocking the UI thread
        if let Some(ref acp_client) = self.acp_client {
            let client = acp_client.clone();
            tokio::spawn(async move {
                if let Err(e) = client.cancel().await {
                    tracing::warn!(error = %e, "ACP cancel failed (session may have ended)");
                }
            });
            // 安全网：记录 cancel 时间，5 秒后如果仍在 loading 则强制清理
            self.session_mgr.current_mut().agent.cancel_sent_at = Some(std::time::Instant::now());
            // ACP 路径：cancel 已发送，UI 清理由后续 Interrupted/Done 事件完成。
            // 不执行强制清理——避免与 ACP server 端事件竞态导致双重清理。
            return;
        }
        // Fallback: direct cancel_token (legacy path, kept for tests)
        if let Some(token) = &self.session_mgr.current_mut().agent.cancel_token {
            token.cancel();
        } else if self.session_mgr.current_mut().ui.loading {
            tracing::warn!("interrupt: 无 cancel_token 但 loading=true，强制清理");
            self.set_loading(false);
            self.session_mgr.current_mut().agent.interaction_prompt = None;
            self.session_mgr.current_mut().agent.pending_hitl_items = None;
            self.session_mgr.current_mut().agent.pending_ask_user = None;
            if let Some(start) = self.session_mgr.current_mut().agent.task_start_time {
                self.session_mgr.current_mut().agent.last_task_duration = Some(start.elapsed());
            }

            // 始终尝试恢复用户文本到输入框（无论 agent 是否已回复）
            if let Some(text) = self
                .session_mgr
                .current_mut()
                .messages
                .last_submitted_text
                .take()
            {
                // 在 view_messages 中定位最后一个 UserBubble 的索引
                let user_msg_idx = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .iter()
                    .rposition(|vm| matches!(vm, MessageViewModel::UserBubble { .. }))
                    .unwrap_or(0);
                self.session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .truncate(user_msg_idx);
                self.session_mgr
                    .current_mut()
                    .messages
                    .ephemeral_notes
                    .retain(|(a, _)| *a < user_msg_idx);
                {
                    let remaining = self
                        .session_mgr
                        .current_mut()
                        .messages
                        .view_messages
                        .clone();
                    let _ = self
                        .session_mgr
                        .current_mut()
                        .messages
                        .render_tx
                        .try_send(RenderEvent::Rebuild(remaining));
                }
                // 截断 origin_messages（回滚 StateSnapshot 扩展的内容）
                let pre_len = self.session_mgr.current_mut().metadata.pre_submit_state_len;
                self.session_mgr
                    .current_mut()
                    .agent
                    .origin_messages
                    .truncate(pre_len);
                // 清除 pipeline 状态
                self.session_mgr.current_mut().messages.pipeline.done();
                let restored = self.session_mgr.current_mut().agent.origin_messages.clone();
                self.session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .restore_completed(restored);
                let mut ta = build_textarea(false);
                ta.insert_str(text.clone());
                self.session_mgr.current_mut().ui.textarea = ta;
                self.session_mgr
                    .current_mut()
                    .messages
                    .pending_messages
                    .clear();
                self.session_mgr.current_mut().metadata.last_human_message = None;
                self.push_system_note(format!(
                    "⚠ {}",
                    self.services.lc.tr("app-interrupted-resumed")
                ));
                self.render_rebuild();
            } else {
                self.push_system_note(format!(
                    "⚠ {}",
                    self.services.lc.tr("app-interrupted-background")
                ));
                self.render_rebuild();
            }
        }
    }

    pub fn set_loading(&mut self, loading: bool) {
        let s = self.active_mut();
        s.ui.loading = loading;
        if loading {
            s.ui.prediction = None;
            s.ui.textarea = build_textarea(true);
            s.spinner_state
                .set_mode(peri_widgets::SpinnerMode::Responding);
        } else {
            s.spinner_state.set_mode(peri_widgets::SpinnerMode::Idle);
            s.agent.cancel_token = None;
        }
    }

    /// 重建输入框（pending_messages 现在由 UI 层直接渲染，不再使用 textarea title）
    pub fn update_textarea_hint(&mut self) {
        // 不再需要更新 textarea title，pending_messages 在输入框上方渲染
    }

    /// 设置当前 Agent 的 ID（用于 AgentDefineMiddleware）
    pub fn set_agent_id(&mut self, id: Option<String>) {
        self.session_mgr.current_mut().agent.agent_id = id;
    }

    /// 获取当前 Agent 的 ID
    pub fn get_agent_id(&self) -> Option<&String> {
        self.session_mgr.current().agent.agent_id.as_ref()
    }

    /// 打开面板（统一处理跨作用域互斥）：关闭所有 manager 中的面板后，放入正确的 manager
    pub fn open_panel(&mut self, state: panel_manager::PanelState) {
        match state.kind().scope() {
            panel_manager::PanelScope::Session => {
                self.global_panels.close();
                self.session_mgr.current_mut().session_panels.close();
                self.session_mgr.current_mut().session_panels.open(state);
            }
            panel_manager::PanelScope::Global => {
                self.global_panels.close();
                self.session_mgr.current_mut().session_panels.close();
                self.global_panels.open(state);
            }
        }
    }

    /// 关闭所有面板（跨所有作用域）
    pub fn close_all_panels(&mut self) {
        self.global_panels.close();
        self.session_mgr.current_mut().session_panels.close();
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

    /// 检查是否有任何交互弹窗处于激活状态（AskUser / HITL / OAuth）。
    /// 弹窗激活时，底部 textarea 应失效——隐藏光标、禁止输入、视觉变暗。
    pub fn is_interaction_popup_active(&self) -> bool {
        self.global_ui.oauth_prompt.is_some()
            || self
                .session_mgr
                .current()
                .agent
                .interaction_prompt
                .is_some()
    }

    /// 将粘贴文本路由到当前激活弹窗的输入区。用于支持 IME 组合输入（macOS
    /// 终端通过 Bracketed Paste 发送组合后的中文），以及常规粘贴操作。
    /// 仅处理 AskUser 弹窗的 custom_input；HITL/OAuth 弹窗无文本输入区，静默丢弃。
    pub fn paste_to_interaction_popup(&mut self, text: &str) {
        if let Some(crate::app::InteractionPrompt::Questions(p)) = self
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .as_mut()
        {
            let q = p.current();
            q.custom_input.insert_text(text);
            q.in_custom_input = true;
        }
    }
}
