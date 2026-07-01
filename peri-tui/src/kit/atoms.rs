//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit StoreState<T> 作为 Copy 句柄的全局状态容器。通过 OnceLock
//! 在运行时初始化，组件通过 use_store(&atom) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Atom<T> = StoreState<T>（保持与设计文档一致的命名）。

use chrono::{DateTime, Utc};
use peri_acp_types::view_model::ViewModel;
use ratatui_kit::prelude::StoreState;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::panel_types::PanelKind;

// ──────────────────────────────────────────────────────────────────────────
// Popup 系统：S7 引入。4 种交互弹窗，由 ACP 事件或全局快捷键触发。
// ──────────────────────────────────────────────────────────────────────────

/// 4 种交互弹窗枚举——由 AcpEvent 或本地操作触发。
///
/// - **Hitl**：来自 `AcpEventData::HitlPending`，工具调用审批
/// - **AskUser**：来自 `AcpEventData::AskUser`，Agent 向用户提问
/// - **Rewind**：来自 `AcpEventData::RewindPreview`，回退预览（也可由双击 Esc 触发）
/// - **OAuth**：来自 `AcpEventData::OauthNeeded`，OAuth 授权
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Hitl,
    AskUser,
    Rewind,
    OAuth,
}

/// 类型别名：将 StoreState 映射为 Atom，保持命名一致性
pub type Atom<T> = StoreState<T>;

/// ACP 状态快照（轻量投影，不含大对象）
#[derive(Debug, Clone, Default)]
pub struct AcpStateSnapshot {
    pub variant: u8, // 0=Idle, 1=Streaming, 2=Modal, 3=Switching
    pub view_count: usize,
    pub is_loading: bool,
    pub popup_active: bool,
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}

/// Session ViewModels 快照
#[derive(Debug, Clone, Default)]
pub struct ViewModelsSnapshot {
    pub committed: Vec<ViewModel>,
    pub current_turn: Vec<ViewModel>,
}

// ── S5：service snapshot 投影类型 ─────────────────────────────────────────

/// 跨 session 共享的服务状态快照——由 `kit::service_snapshot` 后台任务
/// 周期性从 `ServiceRegistry` 派生并写入 `SERVICE_SNAPSHOT` Atom。
///
/// 设计原则：
/// - **只读投影**：所有字段 clone 自 ServiceRegistry 共享字段，不持 &App
/// - **Send+Sync**：可直接写入 Atom（Atom 内部用 RwLock）
/// - **派生而非引用**：provider_name/model_name 从 peri_config 派生，
///   不直接读 ServiceRegistry.provider_name（String 非 Arc，无法跨 task 共享）
#[derive(Debug, Clone, Default)]
pub struct ServiceSnapshot {
    /// 当前工作目录
    pub cwd: String,
    /// 当前 provider 显示名（如 "anthropic"）
    pub provider_name: String,
    /// 当前 model alias（如 "sonnet"）
    pub model_alias: String,
    /// 权限模式字符串（"bypass"/"default"/"accept-edit"/"auto-mode"）
    pub permission_mode: String,
    /// 进程内存（MB）
    pub memory_mb: u64,
    /// 进程 CPU 占用百分比（单核；可超 100 表示多核）
    pub cpu_percent: f32,
    /// MCP 服务池状态
    pub mcp: McpStatusSnapshot,
    /// Cron 任务总数（含已禁用）
    pub cron_total: usize,
    /// Cron 启用任务数
    pub cron_enabled: usize,
}

/// MCP 服务池投影
#[derive(Debug, Clone, Default)]
pub struct McpStatusSnapshot {
    /// 是否已完成初始化（Pending/Initializing/Failed/Ready）
    pub init_phase: McpInitPhase,
    /// 配置总数
    pub total: usize,
    /// 成功连接数
    pub connected: usize,
}

/// MCP 初始化阶段（从 `McpInitStatus` 派生的稳定枚举，避免 kit 依赖中间件层）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpInitPhase {
    #[default]
    Pending,
    Initializing,
    Ready,
    Failed,
}

/// Thread 列表条目投影——从 `peri_agent::thread::ThreadMeta` 派生
#[derive(Debug, Clone, Default)]
pub struct ThreadSummary {
    pub id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub message_count: usize,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Cron 任务条目投影——从 `peri_middlewares::cron::CronTask` 派生
#[derive(Debug, Clone, Default)]
pub struct CronJobSummary {
    pub id: String,
    pub expression: String,
    pub prompt: String,
    pub enabled: bool,
    pub next_fire: Option<DateTime<Utc>>,
}

// ── 全局 Atom 声明（OnceLock 延迟初始化） ──

pub static ACP_STATE: OnceLock<Atom<AcpStateSnapshot>> = OnceLock::new();
pub static VIEW_MODELS: OnceLock<Atom<ViewModelsSnapshot>> = OnceLock::new();
pub static SCROLL_OFFSET: OnceLock<Atom<u16>> = OnceLock::new();

/// 状态栏瞬时高亮计时器
pub static MODEL_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static PROVIDER_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static MODE_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();

/// @mention / slash_hint / popup 激活状态
pub static AT_MENTION_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
pub static SLASH_HINT_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
pub static POPUP_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();

/// 提交通道：InputArea 写入 → submit_consumer 读取 → acp_client.prompt()。
///
/// 用 mpsc 而非 atom 的原因：
/// 1. **背压**：UnboundedSender 在 channel 满时自然阻塞消费者，atom 无此语义。
/// 2. **顺序保证**：每个提交独立成事件，消费者按序处理；atom 会被覆盖丢失。
/// 3. **Send+Sync**：UnboundedSender 可在 #[component] 闭包与 tokio task 间自由 Clone。
pub static SUBMIT_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

// ── S5：service snapshot / panels / threads atoms ─────────────────────────

/// 服务状态快照（CPU/MEM/MCP/Cron/provider/model/cwd/permission_mode）
pub static SERVICE_SNAPSHOT: OnceLock<Atom<ServiceSnapshot>> = OnceLock::new();

/// Thread 列表（ThreadBrowser 面板用）
pub static THREAD_LIST: OnceLock<Atom<Vec<ThreadSummary>>> = OnceLock::new();

/// Cron 任务列表（Cron 面板用）
pub static CRON_JOBS: OnceLock<Atom<Vec<CronJobSummary>>> = OnceLock::new();

/// 当前打开的面板栈（栈顶 = ACTIVE_PANEL）。空 Vec 表示无面板打开。
/// 同 MutexGroup 面板不可同时存在——这个约束由 panel_open/close 命令保证。
pub static OPEN_PANELS: OnceLock<Atom<Vec<PanelKind>>> = OnceLock::new();

/// 当前激活面板（栈顶），快捷渲染用。None = 无面板。
pub static ACTIVE_PANEL: OnceLock<Atom<Option<PanelKind>>> = OnceLock::new();

/// 当前激活弹窗。None = 无弹窗。弹窗优先级高于面板——同时存在时弹窗先消费 Esc。
pub static POPUP_KIND: OnceLock<Atom<Option<PopupKind>>> = OnceLock::new();

/// 初始化所有全局 Atom。
///
/// 必须在 tokio 运行时启动后、任何组件渲染前调用。
pub fn init_atoms() {
    ACP_STATE.get_or_init(|| Atom::new(AcpStateSnapshot::default()));
    VIEW_MODELS.get_or_init(|| Atom::new(ViewModelsSnapshot::default()));
    SCROLL_OFFSET.get_or_init(|| Atom::new(0));
    MODEL_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    PROVIDER_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    MODE_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    AT_MENTION_ACTIVE.get_or_init(|| Atom::new(false));
    SLASH_HINT_ACTIVE.get_or_init(|| Atom::new(false));
    POPUP_ACTIVE.get_or_init(|| Atom::new(false));
    SERVICE_SNAPSHOT.get_or_init(|| Atom::new(ServiceSnapshot::default()));
    THREAD_LIST.get_or_init(|| Atom::new(Vec::new()));
    CRON_JOBS.get_or_init(|| Atom::new(Vec::new()));
    OPEN_PANELS.get_or_init(|| Atom::new(Vec::new()));
    ACTIVE_PANEL.get_or_init(|| Atom::new(None));
    POPUP_KIND.get_or_init(|| Atom::new(None));
    // SUBMIT_TX 由 entry::run_kit_fullscreen 在 build_app_and_acp 之后初始化
    // （需要 mpsc::unbounded_channel 的 rx 配对），不在此处 get_or_init。
}
