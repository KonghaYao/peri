//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit 0.7 AtomStatic<T> + AtomState<T> 作为全局状态容器。
//! 组件通过 use_atom(&ATOM) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Handle<T> = AtomState<T>（供其他文件引用）。

use chrono::{DateTime, Utc};
use peri_acp_types::event_data::{AskUser, HitlPending, OauthNeeded, RewindPreview};
use peri_acp_types::view_model::ViewModel;
use ratatui_kit::prelude::{Atom as AtomStatic, AtomState};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::panel_types::PanelKind;
use crate::kit::rewind_action::RewindAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Hitl,
    AskUser,
    Rewind,
    OAuth,
}

pub type Handle<T> = AtomState<T>;

#[derive(Debug, Clone, Default)]
pub struct AcpStateSnapshot {
    pub variant: u8,
    pub view_count: usize,
    pub is_loading: bool,
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}

#[derive(Debug, Clone)]
pub struct ViewModelsSnapshot {
    pub committed: Arc<[ViewModel]>,
    pub current_turn: Arc<[ViewModel]>,
}

impl Default for ViewModelsSnapshot {
    fn default() -> Self {
        Self {
            committed: Arc::from([]),
            current_turn: Arc::from([]),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceSnapshot {
    pub cwd: String,
    pub provider_name: String,
    pub model_alias: String,
    pub permission_mode: String,
    pub memory_mb: u64,
    pub cpu_percent: f32,
    pub mcp: McpStatusSnapshot,
    pub cron_total: usize,
    pub cron_enabled: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpStatusSnapshot {
    pub init_phase: McpInitPhase,
    pub total: usize,
    pub connected: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpInitPhase {
    #[default]
    Pending,
    Initializing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadSummary {
    pub id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub message_count: usize,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CronJobSummary {
    pub id: String,
    pub expression: String,
    pub prompt: String,
    pub enabled: bool,
    pub next_fire: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookSummary {
    pub event: String,
    pub plugin_name: String,
    pub command: String,
    pub matcher: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub root: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerSummary {
    pub name: String,
    pub status: String,
    pub transport: String,
    pub tools_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SubagentSummary {
    pub agent_id: String,
    pub display_name: String,
    pub is_running: bool,
    pub total_steps: usize,
    pub status_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderSummary {
    pub id: String,
    pub provider_type: String,
    pub is_active: bool,
    pub has_api_key: bool,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryEntry {
    pub path: String,
    pub size_bytes: u64,
    pub modified: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct PredictionState {
    pub text: String,
    pub received_at: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct PendingAttachment {
    pub label: String,
    pub media_type: String,
    pub base64_data: String,
}

pub static PENDING_ATTACHMENTS: OnceLock<Handle<Vec<PendingAttachment>>> = OnceLock::new();

pub static ACP_STATE: AtomStatic<AcpStateSnapshot> =
    AtomStatic::new(|| AcpStateSnapshot::default());
pub static VIEW_MODELS: AtomStatic<ViewModelsSnapshot> =
    AtomStatic::new(|| ViewModelsSnapshot::default());
pub static MODEL_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static PROVIDER_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static MODE_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static AT_MENTION_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
pub static SLASH_HINT_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
pub static SUBMIT_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

pub static SERVICE_SNAPSHOT: AtomStatic<ServiceSnapshot> =
    AtomStatic::new(|| ServiceSnapshot::default());
pub static THREAD_LIST: AtomStatic<Vec<ThreadSummary>> = AtomStatic::new(|| Vec::new());
pub static CRON_JOBS: AtomStatic<Vec<CronJobSummary>> = AtomStatic::new(|| Vec::new());
pub static HOOK_LIST: AtomStatic<Vec<HookSummary>> = AtomStatic::new(|| Vec::new());
pub static PLUGIN_LIST: AtomStatic<Vec<PluginSummary>> = AtomStatic::new(|| Vec::new());
pub static MCP_SERVERS: AtomStatic<Vec<McpServerSummary>> = AtomStatic::new(|| Vec::new());
pub static SUBAGENT_LIST: AtomStatic<Vec<SubagentSummary>> = AtomStatic::new(|| Vec::new());
pub static PROVIDER_LIST: AtomStatic<Vec<ProviderSummary>> = AtomStatic::new(|| Vec::new());
pub static MEMORY_LIST: AtomStatic<Vec<MemoryEntry>> = AtomStatic::new(|| Vec::new());

pub static OPEN_PANELS: AtomStatic<Vec<PanelKind>> = AtomStatic::new(|| Vec::new());
pub static ACTIVE_PANEL: AtomStatic<Option<PanelKind>> = AtomStatic::new(|| None);
pub static POPUP_KIND: AtomStatic<Option<PopupKind>> = AtomStatic::new(|| None);

pub static INPUT_HISTORY: AtomStatic<VecDeque<String>> = AtomStatic::new(|| VecDeque::new());
pub static INPUT_HISTORY_INDEX: AtomStatic<Option<usize>> = AtomStatic::new(|| None);
pub static INPUT_BUFFER: AtomStatic<VecDeque<String>> = AtomStatic::new(|| VecDeque::new());

pub static FILE_LIST: AtomStatic<Vec<String>> = AtomStatic::new(|| Vec::new());
pub static MENTION_PREFIX: AtomStatic<String> = AtomStatic::new(|| String::new());
pub static SLASH_PREFIX: AtomStatic<String> = AtomStatic::new(|| String::new());

pub static MENTION_SELECTED_INDEX: AtomStatic<usize> = AtomStatic::new(|| 0);
pub static SLASH_SELECTED_INDEX: AtomStatic<usize> = AtomStatic::new(|| 0);

pub static DIFF_VISIBLE: AtomStatic<bool> = AtomStatic::new(|| false);

pub static REWIND_PREVIEW: AtomStatic<Option<RewindPreview>> = AtomStatic::new(|| None);
pub static OAUTH_INFO: AtomStatic<Option<OauthNeeded>> = AtomStatic::new(|| None);
pub static HITL_PENDING: AtomStatic<Option<HitlPending>> = AtomStatic::new(|| None);
pub static ASK_USER_PENDING: AtomStatic<Option<AskUser>> = AtomStatic::new(|| None);

pub static LAST_ESC_TIME: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static QUIT_PENDING_SINCE: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);

pub static REWIND_ACTION_TX: OnceLock<UnboundedSender<RewindAction>> = OnceLock::new();
pub static THREAD_LOAD_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

pub static PERI_CONFIG_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::RwLock<crate::config::PeriConfig>>,
> = OnceLock::new();
pub static PERMISSION_MODE_HANDLE: OnceLock<
    std::sync::Arc<peri_middlewares::prelude::SharedPermissionMode>,
> = OnceLock::new();
pub static CRON_SCHEDULER_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>,
> = OnceLock::new();

pub static ACP_COMMANDS: AtomStatic<Vec<String>> = AtomStatic::new(|| Vec::new());
pub static SKILL_NAMES: AtomStatic<Vec<String>> = AtomStatic::new(|| Vec::new());
/// ACP 服务器下发的可用 slash 命令列表（含 skills）。
/// 键 = 命令名（不含 / 前缀），值 = 描述。
/// 由 kit notifier 在收到 `SessionUpdate::AvailableCommandsUpdate` 后写入。
pub static AVAILABLE_SLASH_COMMANDS: AtomStatic<Vec<(String, String)>> =
    AtomStatic::new(|| Vec::new());
pub static WIZARD_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
pub static PREDICTION: AtomStatic<PredictionState> = AtomStatic::new(|| PredictionState::default());
pub static INPUT_AREA_ESC_PREFIX: AtomStatic<bool> = AtomStatic::new(|| false);

pub fn init_atoms() {
    PENDING_ATTACHMENTS.get_or_init(|| Handle::new(Vec::new()));
}
