//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit StoreState<T> 作为 Copy 句柄的全局状态容器。通过 OnceLock
//! 在运行时初始化，组件通过 use_store(&atom) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Atom<T> = StoreState<T>（保持与设计文档一致的命名）。

use chrono::{DateTime, Utc};
use peri_acp_types::event_data::{OauthNeeded, RewindPreview};
use peri_acp_types::view_model::ViewModel;
use ratatui_kit::prelude::StoreState;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::panel_types::PanelKind;
use crate::kit::rewind_action::RewindAction;

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
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}

/// Session ViewModels 快照
///
/// I20-B：`committed` 用 `Arc<[ViewModel]>` 而非 `Vec<ViewModel>`——
/// `push_view_models` 在每个 streaming chunk（TextChunk/ReasoningChunk/ToolStarted/
/// ToolEnded）上都会 clone 一份快照写入 atom，长时间会话中每次 clone O(n)
/// 会造成严重性能问题。改 Arc 后 clone O(1)，只有 ViewCommit/TurnDone 等
/// 真正修改 committed 的事件才重建 Arc（O(n)，但稀少）。
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

/// H1b：Hook 条目投影——从 `peri_middlewares::hooks::RegisteredHook` 派生
///
/// service_snapshot tick 把 SnapshotSource.hooks（启动时一次性派生）拷贝到
/// `HOOK_LIST` atom。Hooks 面板从这里读真实数据。
#[derive(Debug, Clone, Default)]
pub struct HookSummary {
    pub event: String,
    pub plugin_name: String,
    pub command: String,
    pub matcher: Option<String>,
}

/// H1c：Plugin 条目投影——从 `peri_middlewares::plugin::LoadedPlugin` 派生
#[derive(Debug, Clone, Default)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub root: String,
    pub description: String,
}

/// H1d：MCP server 详细状态投影——从 `McpClientPool.all_server_infos` 派生
#[derive(Debug, Clone, Default)]
pub struct McpServerSummary {
    pub name: String,
    pub status: String,
    pub transport: String,
    pub tools_count: usize,
}

/// H1e：SubAgent 运行时状态投影——从 `SubAgentStatusMap` 派生
#[derive(Debug, Clone, Default)]
pub struct SubagentSummary {
    pub agent_id: String,
    pub display_name: String,
    pub is_running: bool,
    pub total_steps: usize,
    pub status_text: String,
}

/// H1f：Provider 配置投影——从 `PeriConfig.providers` 派生
#[derive(Debug, Clone, Default)]
pub struct ProviderSummary {
    pub id: String,
    pub provider_type: String,
    pub is_active: bool,
    pub has_api_key: bool,
    pub base_url: Option<String>,
}

/// H1h：Memory 文件投影——从 ~/.claude/memory 扫描派生
#[derive(Debug, Clone, Default)]
pub struct MemoryEntry {
    pub path: String,
    pub size_bytes: u64,
    pub modified: Option<DateTime<Utc>>,
}

// ── 全局 Atom 声明（OnceLock 延迟初始化） ──

pub static ACP_STATE: OnceLock<Atom<AcpStateSnapshot>> = OnceLock::new();
pub static VIEW_MODELS: OnceLock<Atom<ViewModelsSnapshot>> = OnceLock::new();

/// 状态栏瞬时高亮计时器
pub static MODEL_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static PROVIDER_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static MODE_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();

/// @mention / slash_hint / popup 激活状态
pub static AT_MENTION_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
pub static SLASH_HINT_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
// I19-C：POPUP_ACTIVE 已退役——dead atom（open/close_popup 从不同步）。
// 改读 POPUP_KIND.is_some() 获取真实弹窗状态。

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

/// H1b：Hooks 列表（Hooks 面板用）
pub static HOOK_LIST: OnceLock<Atom<Vec<HookSummary>>> = OnceLock::new();

/// H1c：Plugin 列表（Plugin 面板用）
pub static PLUGIN_LIST: OnceLock<Atom<Vec<PluginSummary>>> = OnceLock::new();

/// H1d：MCP server 详细列表（Mcp 面板用，比 SERVICE_SNAPSHOT.mcp 更详细）
pub static MCP_SERVERS: OnceLock<Atom<Vec<McpServerSummary>>> = OnceLock::new();

/// H1e：SubAgent 运行时列表（Agent 面板用）
pub static SUBAGENT_LIST: OnceLock<Atom<Vec<SubagentSummary>>> = OnceLock::new();

/// H1f：Provider 配置列表（Login 面板用）
pub static PROVIDER_LIST: OnceLock<Atom<Vec<ProviderSummary>>> = OnceLock::new();

/// H1h：Memory 文件列表（Memory 面板用）
pub static MEMORY_LIST: OnceLock<Atom<Vec<MemoryEntry>>> = OnceLock::new();

/// 当前打开的面板栈（栈顶 = ACTIVE_PANEL）。空 Vec 表示无面板打开。
/// 同 MutexGroup 面板不可同时存在——这个约束由 panel_open/close 命令保证。
pub static OPEN_PANELS: OnceLock<Atom<Vec<PanelKind>>> = OnceLock::new();

/// 当前激活面板（栈顶），快捷渲染用。None = 无面板。
pub static ACTIVE_PANEL: OnceLock<Atom<Option<PanelKind>>> = OnceLock::new();

/// 当前激活弹窗。None = 无弹窗。弹窗优先级高于面板——同时存在时弹窗先消费 Esc。
pub static POPUP_KIND: OnceLock<Atom<Option<PopupKind>>> = OnceLock::new();

/// 输入历史栈——按提交时间顺序（旧 → 新）。最多 100 条，超出后从头部丢弃。
///
/// 由 `submit_consumer` 在 prompt 提交后 `push_history(text)` 写入；
/// InputArea 用 Up/Down 浏览，按 Esc 或重新输入文本回到底部。
pub static INPUT_HISTORY: OnceLock<Atom<VecDeque<String>>> = OnceLock::new();

/// 输入历史浏览指针——`Some(i)` 表示当前正在浏览 `INPUT_HISTORY[i]`，
/// `None` 表示正在编辑新文本（非历史浏览状态）。
pub static INPUT_HISTORY_INDEX: OnceLock<Atom<Option<usize>>> = OnceLock::new();

/// 输入缓冲区——agent loading 时用户按 Enter 的输入按顺序排队。
///
/// 设计：当 `ACP_STATE.is_loading == true` 时 InputArea 把 Enter 提交的文本
/// push_back 到此队列；TurnDone 事件触发时由 acp_events 按顺序 drain 并
/// 通过 SUBMIT_TX 重新提交。这样用户在 agent 运行期间可以连续输入下一条
/// 指令而不会丢失。
///
/// 上限：保留最近 32 条，超出从头部丢弃（防止无限增长）。
pub static INPUT_BUFFER: OnceLock<Atom<VecDeque<String>>> = OnceLock::new();

/// 当前工作目录下浅扫的文件相对路径列表（用于 @mention 补全）。
///
/// 由 service_snapshot 每 2s 刷新一次：扫描 cwd 顶层 + 1 层子目录，
/// 过滤常见忽略目录（.git / node_modules / target / dist 等），最多 500 条。
/// InputArea 渲染时基于 `MENTION_PREFIX` 过滤后传给 MentionPopup。
pub static FILE_LIST: OnceLock<Atom<Vec<String>>> = OnceLock::new();

/// @mention 当前匹配的文件名前缀（用户输入 @ 之后的字符）。
/// 由 InputArea 在用户输入 @ 时写入；MentionPopup 用它过滤文件列表。
pub static MENTION_PREFIX: OnceLock<Atom<String>> = OnceLock::new();

/// slash 命令当前匹配前缀（用户输入 / 之后的字符）。
pub static SLASH_PREFIX: OnceLock<Atom<String>> = OnceLock::new();

/// I18-C：MentionPopup 当前选中项索引（跨组件共享，让 InputArea Enter 读取真实选中）。
pub static MENTION_SELECTED_INDEX: OnceLock<Atom<usize>> = OnceLock::new();

/// I18-C：SlashCompletion 当前选中项索引（跨组件共享，让 InputArea Enter 读取真实选中）。
pub static SLASH_SELECTED_INDEX: OnceLock<Atom<usize>> = OnceLock::new();

/// I19-B：diff 视图展开开关（Ctrl+O toggle），传给 view_render::render_v2_vm。
pub static DIFF_VISIBLE: OnceLock<Atom<bool>> = OnceLock::new();

// ── S10：Rewind 系统 ──────────────────────────────────────────────────────

/// 当前 rewind 预览数据——由 AcpEvent::RewindPreview 写入；RewindPopup 读取。
/// 双击 Esc 触发 popup 时若此 atom 为 None，popup 显示"无可回退"占位。
pub static REWIND_PREVIEW: OnceLock<Atom<Option<RewindPreview>>> = OnceLock::new();

/// I20-D：OAuth 授权数据——由 AcpEvent::OauthNeeded 写入；OAuthPopup 读取。
/// popup 关闭时清空，避免下次打开仍显示陈旧数据。
pub static OAUTH_INFO: OnceLock<Atom<Option<OauthNeeded>>> = OnceLock::new();

/// 双击 Esc 检测——记录最近一次 Esc 按下时间。
/// event_handlers 在 Esc 时检查：距上次 < 500ms 且无 popup → open_popup(Rewind)。
pub static LAST_ESC_TIME: OnceLock<Atom<Option<Instant>>> = OnceLock::new();

/// Rewind 指令通道：RewindPopup 确认/取消 → rewind_consumer → AcpClient。
///
/// 与 SUBMIT_TX 同模式：用 mpsc 而非 atom 以保证顺序 + Send+Sync。
/// Confirm 携带 `target_message_id` + `revert_files`，consumer 调
/// `session/execute-command` (command="/rewind") RPC。
pub static REWIND_ACTION_TX: OnceLock<UnboundedSender<RewindAction>> = OnceLock::new();

/// H3：thread 切换通道——ThreadBrowser Enter → thread_load_consumer → AcpClient.load_session。
///
/// 设计同 SUBMIT_TX：mpsc 保证 Send+Sync + 顺序；消费者在 entry.rs spawn。
/// String = thread_id（即 SQLite Thread 表主键；ACP server 把它直接当 sessionId 用）。
/// 切换成功后 ACP server 推送 view-commit 通知 → kit_notifier → VIEW_MODELS atom 自动刷新。
pub static THREAD_LOAD_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

/// H2：全局 PeriConfig 共享句柄（非 atom——直接 write 反映到所有读取者）。
///
/// ModelPanel / ConfigPanel 等需要修改本地配置（active_alias / permission_mode 等）
/// 的组件直接 read 此 OnceLock 拿到 `Arc<RwLock<PeriConfig>>` 副本，然后 write 内部字段。
/// service_snapshot 任务每 2s 派生 `SERVICE_SNAPSHOT` atom，会自动捕获变化并刷新 UI。
///
/// 注意：ACP server 持有同一 Arc，所以这里 write 后 server 端立即可见——无需额外同步。
pub static PERI_CONFIG_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::RwLock<crate::config::PeriConfig>>,
> = OnceLock::new();

/// H1a：全局 PermissionMode 共享句柄——Config 面板切换 permission_mode 时直接 store。
///
/// 来自 `ServiceRegistry.permission_mode: Arc<SharedPermissionMode>`。ConfigPanel /
/// Status Bar 均通过此句柄读写。service_snapshot tick 派生
/// SERVICE_SNAPSHOT.permission_mode 字符串投影刷新 UI。
pub static PERMISSION_MODE_HANDLE: OnceLock<
    std::sync::Arc<peri_middlewares::prelude::SharedPermissionMode>,
> = OnceLock::new();

/// H1g：全局 CronScheduler 共享句柄——Cron 面板 toggle/delete 时直接调用。
///
/// 来自 `SnapshotSource.cron_scheduler: Arc<Mutex<CronScheduler>>`，由 entry.rs
/// 启动 service_snapshot 时同步 set 到此 OnceLock。CronPanel 用它直接执行
/// `scheduler.toggle(id)` / `scheduler.remove(id)`——service_snapshot 下次 tick
/// 自动派生新列表写入 CRON_JOBS atom 刷新 UI（延迟 ≤2s）。
pub static CRON_SCHEDULER_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>,
> = OnceLock::new();

/// I17-B：Setup Wizard 触发开关。
///
/// 首次启动时若 `needs_setup() == true`（无 Provider 配置），entry.rs 设置此
/// atom 为 true，触发 `kit/setup_wizard.rs` 渲染引导界面。用户按 q/Esc/Enter
/// 关闭 wizard 后写入 false（即使未配置 Provider 也允许进入主界面，避免
/// 首次启动锁死）。
pub static WIZARD_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();

/// 初始化所有全局 Atom。
///
/// 必须在 tokio 运行时启动后、任何组件渲染前调用。
pub fn init_atoms() {
    ACP_STATE.get_or_init(|| Atom::new(AcpStateSnapshot::default()));
    VIEW_MODELS.get_or_init(|| Atom::new(ViewModelsSnapshot::default()));
    MODEL_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    PROVIDER_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    MODE_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    AT_MENTION_ACTIVE.get_or_init(|| Atom::new(false));
    SLASH_HINT_ACTIVE.get_or_init(|| Atom::new(false));
    SERVICE_SNAPSHOT.get_or_init(|| Atom::new(ServiceSnapshot::default()));
    THREAD_LIST.get_or_init(|| Atom::new(Vec::new()));
    CRON_JOBS.get_or_init(|| Atom::new(Vec::new()));
    HOOK_LIST.get_or_init(|| Atom::new(Vec::new()));
    PLUGIN_LIST.get_or_init(|| Atom::new(Vec::new()));
    MCP_SERVERS.get_or_init(|| Atom::new(Vec::new()));
    SUBAGENT_LIST.get_or_init(|| Atom::new(Vec::new()));
    PROVIDER_LIST.get_or_init(|| Atom::new(Vec::new()));
    MEMORY_LIST.get_or_init(|| Atom::new(Vec::new()));
    WIZARD_ACTIVE.get_or_init(|| Atom::new(false));
    OPEN_PANELS.get_or_init(|| Atom::new(Vec::new()));
    ACTIVE_PANEL.get_or_init(|| Atom::new(None));
    POPUP_KIND.get_or_init(|| Atom::new(None));
    INPUT_HISTORY.get_or_init(|| Atom::new(VecDeque::new()));
    INPUT_HISTORY_INDEX.get_or_init(|| Atom::new(None));
    INPUT_BUFFER.get_or_init(|| Atom::new(VecDeque::new()));
    FILE_LIST.get_or_init(|| Atom::new(Vec::new()));
    MENTION_PREFIX.get_or_init(|| Atom::new(String::new()));
    SLASH_PREFIX.get_or_init(|| Atom::new(String::new()));
    MENTION_SELECTED_INDEX.get_or_init(|| Atom::new(0));
    SLASH_SELECTED_INDEX.get_or_init(|| Atom::new(0));
    DIFF_VISIBLE.get_or_init(|| Atom::new(false));
    REWIND_PREVIEW.get_or_init(|| Atom::new(None));
    OAUTH_INFO.get_or_init(|| Atom::new(None));
    LAST_ESC_TIME.get_or_init(|| Atom::new(None));
    // SUBMIT_TX 由 entry::run_kit_fullscreen 在 build_app_and_acp 之后初始化
    // （需要 mpsc::unbounded_channel 的 rx 配对），不在此处 get_or_init。
    // REWIND_ACTION_TX 同理——需要 rx 配对给 spawn_rewind_consumer。
}
