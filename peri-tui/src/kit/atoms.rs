//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit 0.7 AtomStatic<T> + AtomState<T> 作为全局状态容器。
//! 组件通过 use_atom(&ATOM) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Handle<T> = AtomState<T>（供其他文件引用）。
//!
//! Channel 约定：
//! - SUBMIT_TX: event_handlers 按键 → submit_consumer 消费
//! - CANCEL_TX: event_handlers Ctrl+C → cancel_consumer 消费
//! - REWIND_ACTION_TX: rewind popup → rewind_consumer
//! - THREAD_LOAD_TX: thread browser → thread_load_consumer

use crate::kit::tui_render_unit::{FoldKey, FoldState, TuiRenderUnit};
use crate::kit::workflow_snapshot::WorkflowSnapshot;
use chrono::{DateTime, Utc};
use peri_acp_types::event_data::{AskUser, HitlPending, OauthNeeded, RewindPreview};
use ratatui_kit::prelude::{Atom as AtomStatic, AtomState};
use ratatui_kit::ratatui::layout::Rect;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::panel_types::PanelKind;
use crate::app::setup_wizard::SetupWizardState;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::hitl_response::HitlResponseAction;
use crate::kit::image_safety::{ImageMeta, PathGrade};
use crate::kit::rewind_action::RewindAction;
use crate::kit::slash_projection::SlashCommandEntry;
use crate::kit::submit_request::SubmitRequest;
use image::DynamicImage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Hitl,
    AskUser,
    Rewind,
    OAuth,
    Confirm,
    /// 下载进度弹窗（主题下载）
    Download,
    /// 状态栏模型段点击弹出的 alias 快速切换弹窗
    ModelQuickSwitch,
}

pub type Handle<T> = AtomState<T>;

#[derive(Debug, Clone, PartialEq, Default)]
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
    pub items: im::Vector<TuiRenderUnit>,
    pub generation: u64,
}

impl Default for ViewModelsSnapshot {
    fn default() -> Self {
        Self {
            items: im::Vector::new(),
            generation: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceSnapshot {
    pub cwd: String,
    pub provider_name: String,
    pub model_alias: String,
    pub model_name: String,
    /// 当前 active profile 的推理力度（low/medium/high/xhigh/max）
    pub effort: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginViewTab {
    #[default]
    Installed = 0,
    Discover = 1,
    Marketplaces = 2,
    Errors = 3,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub root: String,
    pub description: String,
    pub marketplace: String,
    pub author: Option<String>,
    pub skills_count: usize,
    pub commands_count: usize,
    pub agents_count: usize,
    pub mcp_count: usize,
    pub install_scope: String,
    pub load_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerSummary {
    pub name: String,
    pub version: Option<String>,
    pub status: String,
    /// 仅用于 MCP 面板的安全、单行失败摘要；完整诊断写入 tracing 日志。
    pub error_summary: Option<String>,
    pub transport: String,
    pub tools_count: usize,
    /// OAuth 待授权标记（oauth_status == NeedsAuthorization）：面板据此
    /// 显示"授权"按钮（Enter 触发 mcp/oauth_start）。
    pub needs_auth: bool,
    /// 服务器 URL（HTTP 传输），详情视图展示用。
    pub url: Option<String>,
    /// 最近一次持久化 MCP cache 结果：hit/live_fetch；None 表示尚无记录。
    pub cache_status: Option<String>,
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
    /// 最近一次预测的会话摘要（spinner 名言位优先显示）
    pub summary: Option<String>,
    pub received_at: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct PendingAttachment {
    pub label: String,
    pub media_type: String,
    pub base64_data: String,
}

/// 待发送附件（§10 composer footer `@ N files` 消费）。
/// 早期为 OnceLock<Handle<...>>（无消费方）；Slice 3 起输入区订阅显示计数，
/// 改为 AtomStatic 以获得订阅唤醒（写入自动重渲染）。
pub static PENDING_ATTACHMENTS: AtomStatic<Vec<PendingAttachment>> = AtomStatic::new(Vec::new);

pub static ACP_STATE: AtomStatic<AcpStateSnapshot> = AtomStatic::new(AcpStateSnapshot::default);
/// 用户手动折叠覆盖表（Slice 2）——key 为 entry 身份（message_id/tool_id/agent_id），
/// value 为手动 fold 目标。存在即视为 `user_modified=true`：折叠 pass
/// （`acp_events/render.rs::apply_fold_pass`）必须跳过这些 entry 的自动策略，
/// 并在每次快照重建后复写 fold（流式重建后手动选择依然生效）。
/// session 复位（push_view_models_for_reset）时清空。
pub static FOLD_OVERRIDES: AtomStatic<HashMap<FoldKey, FoldState>> = AtomStatic::new(HashMap::new);
/// [Slice 2 §3.4 焦点单一事实源] 消息区 entry 导航焦点——一次写入即完整表达
/// 导航事实（slot + key），取代旧「局部 entry_focus + 共享折叠键 atom」
/// 双轨（仲裁/渲染读局部、外部清除只写共享，收敛依赖下一帧 effect → 窗口期）。
/// 所有设焦点/清除写点收口在 message_area（写锁内派生 key），外部组件（输入区
/// 点击 / session 复位）事件边界同步清 None——无异步收敛。
/// key 为派生值：foldable entry 有值；无折叠能力 entry / request_id 缺失的
/// interaction 合法 `key: None`（slot 仍表达「焦点在消息区」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedEntry {
    pub slot: usize,
    pub key: Option<FoldKey>,
}

pub static FOCUSED_ENTRY: AtomStatic<Option<FocusedEntry>> = AtomStatic::new(|| None);
/// 终端能力（启动时由 entry.rs 探测一次写入；默认全能力——未探测场景不做剥离/降级）。
/// 渲染层按此决定 NO_COLOR 剥离 pass 与符号/italic/truecolor 降级。
pub static TERMINAL_CAPS: AtomStatic<crate::kit::terminal_caps::TerminalCaps> =
    AtomStatic::new(crate::kit::terminal_caps::TerminalCaps::default);
/// loading 会话 epoch 计数器。每次 submit_consumer 发起新的 agent prompt 时递增。
/// message_area 据此检测新的 loading 会话，即便 is_loading 的 false→true 过渡在
/// 同一渲染周期内完成（如 drain_input_buffer 的立即续跑）也能可靠感知。
pub static LOADING_EPOCH: AtomStatic<u64> = AtomStatic::new(|| 0u64);
pub static VIEW_MODELS: AtomStatic<ViewModelsSnapshot> =
    AtomStatic::new(ViewModelsSnapshot::default);
pub static MODEL_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static PROVIDER_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static MODE_HIGHLIGHT_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static AT_MENTION_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
pub static SLASH_HINT_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
pub static SUBMIT_TX: OnceLock<UnboundedSender<SubmitRequest>> = OnceLock::new();
pub static CANCEL_TX: OnceLock<UnboundedSender<()>> = OnceLock::new();
/// /exit 命令：submit_consumer 设为 true，app_shell use_effect 消费并调用 exit_fn()。
pub static EXIT_REQUESTED: AtomStatic<bool> = AtomStatic::new(|| false);
pub static RESIZE_TX: OnceLock<UnboundedSender<u16>> = OnceLock::new();

pub static SERVICE_SNAPSHOT: AtomStatic<ServiceSnapshot> =
    AtomStatic::new(ServiceSnapshot::default);
pub static THREAD_LIST: AtomStatic<Vec<ThreadSummary>> = AtomStatic::new(Vec::new);
pub static CRON_JOBS: AtomStatic<Vec<CronJobSummary>> = AtomStatic::new(Vec::new);
pub static HOOK_LIST: AtomStatic<Vec<HookSummary>> = AtomStatic::new(Vec::new);
pub static PLUGIN_LIST: AtomStatic<Vec<PluginSummary>> = AtomStatic::new(Vec::new);
/// Discover 搜索结果的临时存储（plugin-search-result 事件写入）。
pub static PLUGIN_SEARCH_RESULTS: AtomStatic<Vec<PluginSummary>> = AtomStatic::new(Vec::new);
pub static MCP_SERVERS: AtomStatic<Vec<McpServerSummary>> = AtomStatic::new(Vec::new);
pub static SUBAGENT_LIST: AtomStatic<Vec<SubagentSummary>> = AtomStatic::new(Vec::new);
pub static PROVIDER_LIST: AtomStatic<Vec<ProviderSummary>> = AtomStatic::new(Vec::new);
pub static MEMORY_LIST: AtomStatic<Vec<MemoryEntry>> = AtomStatic::new(Vec::new);

/// Todo 列表数据（来自 ACP SessionUpdate::Plan）
pub static TODO_ITEMS: AtomStatic<Vec<crate::kit::message_area::TodoItem>> =
    AtomStatic::new(Vec::new);

pub static OPEN_PANELS: AtomStatic<Vec<PanelKind>> = AtomStatic::new(Vec::new);
pub static ACTIVE_PANEL: AtomStatic<Option<PanelKind>> = AtomStatic::new(|| None);
/// §6.7 subagent 详情 pane 的选中 agent id——焦点在 `TuiSubAgentGroup` 上按
/// Enter 时写入（mod.rs 焦点分派），SubAgentDetail 面板按此从 VIEW_MODELS
/// 扫描嵌套 `view_models` 渲染。Esc 关闭面板后保留（重开仍显示同一 agent）。
pub static SELECTED_SUBAGENT_ID: AtomStatic<Option<String>> = AtomStatic::new(|| None);
pub static POPUP_KIND: AtomStatic<Option<PopupKind>> = AtomStatic::new(|| None);
/// 当前弹窗的屏幕矩形（上一帧渲染写入）——供鼠标遮挡判定区分「弹窗内/外」：
/// 居中弹窗（HITL 授权等）只覆盖屏幕中部，弹窗外消息区滚轮放行（见
/// mouse_router::occludes_scroll）。写入用 write_no_update（判定读取不依赖
/// 订阅唤醒，与 PANEL_SCROLL_OWNER 同模式）；无弹窗/自定位小层（ModelQuickSwitch
/// 不登记矩形）时为 None → 保守遮挡。
pub static POPUP_AREA: AtomStatic<Option<Rect>> = AtomStatic::new(|| None);
/// 当前激活面板的整体屏幕矩形（上一帧渲染写入，PanelOverlay 的 AreaTracker
/// 回填）——供鼠标遮挡判定区分「面板内/外」：面板打开时，面板区域外（消息区
/// 可见部分）滚轮放行给 chat 滚动（见 mouse_router::occludes_scroll）。
/// 写入用 write_no_update；无面板/尚未渲染时为 None → 保守遮挡。
pub static PANEL_AREA: AtomStatic<Option<Rect>> = AtomStatic::new(|| None);
/// 面板滚轮仲裁注册表：当前激活面板的滚动槽位（每帧由面板渲染体覆盖写入，
/// 见 panel_scroll.rs）。写入用 write_no_update（仲裁读取不依赖订阅唤醒）。
pub static PANEL_SCROLL_OWNER: AtomStatic<Option<crate::kit::panel_scroll::PanelScrollOwner>> =
    AtomStatic::new(|| None);
/// 面板滚轮仲裁节流器（复用消息区 ScrollThrottle 与 scroll_fps 配置）。
pub(crate) static PANEL_SCROLL_THROTTLE: AtomStatic<
    crate::kit::message_area::scroll::ScrollThrottle,
> = AtomStatic::new(crate::kit::message_area::scroll::ScrollThrottle::default);
/// 模型快速切换弹窗锚点（屏幕坐标：状态栏模型段起点 (x, y)）。
/// StatusBarRow1 在 open_popup(ModelQuickSwitch) 前写入，弹窗组件读取后
/// 自定位到锚点上方（非居中大弹窗）。
pub static MODEL_SWITCH_ANCHOR: AtomStatic<Option<(u16, u16)>> = AtomStatic::new(|| None);

pub static INPUT_HISTORY: AtomStatic<VecDeque<String>> = AtomStatic::new(VecDeque::new);
pub static INPUT_HISTORY_INDEX: AtomStatic<Option<usize>> = AtomStatic::new(|| None);
/// 进入历史模式时保存的用户当前输入文本草稿。
pub static DRAFT: AtomStatic<Option<String>> = AtomStatic::new(|| None);
pub static INPUT_BUFFER: AtomStatic<VecDeque<String>> = AtomStatic::new(VecDeque::new);
/// 取消时需恢复到输入框的文本。TurnInterrupted 零产出时写入，input_area 消费后清空。
/// 使用非 atom 存储（OnceLock + Mutex）避免 render body 中写 atom 产生自激回路。
/// TurnInterrupted 写入后递增 RENDER_HEARTBEAT 触发重渲染，input_area 消费文本并清空。
pub static INPUT_RESTORE_TEXT: std::sync::OnceLock<parking_lot::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

pub static FILE_LIST: AtomStatic<Vec<String>> = AtomStatic::new(Vec::new);
pub static MENTION_PREFIX: AtomStatic<String> = AtomStatic::new(String::new);
pub static SLASH_PREFIX: AtomStatic<String> = AtomStatic::new(String::new);

pub static MENTION_SELECTED_INDEX: AtomStatic<usize> = AtomStatic::new(|| 0);
pub static SLASH_SELECTED_INDEX: AtomStatic<usize> = AtomStatic::new(|| 0);

pub static REWIND_PREVIEW: AtomStatic<Option<RewindPreview>> = AtomStatic::new(|| None);

/// 回退目标 user 消息文本暂存——候选 Enter 时写入，RewindCompleted 到达后
/// 消费回填输入框；任何失败/取消路径清空。
pub static REWIND_TARGET_TEXT: AtomStatic<Option<String>> = AtomStatic::new(|| None);

/// `session/rewind-preview` 返回的内容指纹。执行 RPC 必须原样带回；任何新的
/// preview 都先清空旧值，避免用户确认已经过期的文件影响范围。
pub static REWIND_PREVIEW_FINGERPRINT: AtomStatic<Option<String>> = AtomStatic::new(|| None);

/// 文件回退预算状态——候选 Enter 后由 rewind_consumer 写入：
/// `Idle` = 未进入预算阶段（候选视图）；`Executing` = 预算为空自动执行或
/// 用户确认后执行中（弹窗显示"正在回退…"）；`Files(v)` = 待用户确认的预算。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RewindBudgetState {
    #[default]
    Idle,
    Executing,
    Files(Vec<RewindFileChange>),
}

/// 单个文件回退预算条目（服务端 `session/rewind-preview` 响应元素）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RewindFileChange {
    pub path: String,
    pub kind: String,
}

pub static REWIND_BUDGET_STATE: AtomStatic<RewindBudgetState> =
    AtomStatic::new(|| RewindBudgetState::Idle);

/// 候选查询失败信息（Option<String> 错误文案）；None = 查询中或未查询。
pub static REWIND_QUERY_ERROR: AtomStatic<Option<String>> = AtomStatic::new(|| None);

/// 候选查询代次——`spawn_candidates_query` 每次递增并捕获；响应到达时与
/// 当前代次比对，不一致（已发起新查询）则丢弃，防止慢响应覆盖新数据
/// （P1 竞态防护）。
pub static REWIND_QUERY_GEN: AtomStatic<u64> = AtomStatic::new(|| 0);

pub static OAUTH_INFO: AtomStatic<Option<OauthNeeded>> = AtomStatic::new(|| None);
pub static HITL_PENDING: AtomStatic<Option<HitlPending>> = AtomStatic::new(|| None);
pub static ASK_USER_PENDING: AtomStatic<Option<AskUser>> = AtomStatic::new(|| None);
/// Elicitation RequestId 临时存储——notifier 写入，popup Confirm/Esc 读取后通过 consumer 发回 ACP。
pub static ASK_USER_REQUEST_ID: AtomStatic<Option<String>> = AtomStatic::new(|| None);

/// HITL RequestPermission 的 RequestId 临时存储——notifier 写入，
/// hitl_popup 读取后通过 HITL_RESPONSE_TX 发回 hitl_response_consumer。
pub static HITL_REQUEST_ID: AtomStatic<Option<String>> = AtomStatic::new(|| None);

pub static LAST_ESC_TIME: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
pub static QUIT_PENDING_SINCE: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);
/// 防重入：记录上一次 Ctrl+C 事件被处理的时间。同一次按键在 200ms 内重复分发则忽略。
/// 这是防御性保护，防止 ratatui-kit 在事件处理 → atom 写入 → 重渲染过程中
/// 将同一事件二次分发，导致 FirstQuit → 立即 Quit 的 race condition。
pub static LAST_CTRL_C_PROCESSED: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);

pub static REWIND_ACTION_TX: OnceLock<UnboundedSender<RewindAction>> = OnceLock::new();
pub static ASK_USER_RESPONSE_TX: OnceLock<UnboundedSender<AskUserResponseAction>> = OnceLock::new();
pub static HITL_RESPONSE_TX: OnceLock<UnboundedSender<HitlResponseAction>> = OnceLock::new();
pub static THREAD_LOAD_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

pub static PERI_CONFIG_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::RwLock<crate::config::PeriConfig>>,
> = OnceLock::new();
/// 配置源句柄（读写路径决策的唯一事实源）：启动早期 `ConfigSource::load_lenient`
/// 构建一次，TUI 与 ACP 共享同一 `Arc`；所有保存经
/// [`crate::config::save_effective`] 走此句柄。
pub static CONFIG_SOURCE_HANDLE: OnceLock<std::sync::Arc<crate::config::ConfigSource>> =
    OnceLock::new();
/// TUI 渲染配置共享句柄（仅 UI 字段，与 PERI_CONFIG_HANDLE 独立）
pub static TUI_CONFIG_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::RwLock<crate::config::TuiConfig>>,
> = OnceLock::new();
pub static PERMISSION_MODE_HANDLE: OnceLock<
    std::sync::Arc<peri_acp_types::permission::SharedPermissionMode>,
> = OnceLock::new();
pub static CRON_SCHEDULER_HANDLE: OnceLock<
    std::sync::Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>,
> = OnceLock::new();
/// ACP 客户端全局句柄——供 Plugin Panel 等面板调用 send_raw_request。
/// 在 entry.rs 中 acp_client 就绪后 set。
pub static ACP_CLIENT_HANDLE: OnceLock<std::sync::Arc<crate::acp_client::client::AcpTuiClient>> =
    OnceLock::new();
/// TUI 面板直读的 MCP 连接池句柄（`spawn_mcp_init` 创建后 set，C 类豁免）。
/// OAuth 授权完成后（`handle_oauth_completed`）据此触发 reconnect——
/// 从共享凭证文件恢复连接，面板状态随之刷新。
pub static MCP_PANEL_POOL: OnceLock<std::sync::Arc<peri_middlewares::mcp::McpClientPool>> =
    OnceLock::new();
/// i18n 语言版本计数器——语言切换时递增，订阅此 atom 的组件自动重渲染。
/// LcRegistry 本体存于 thread_local!（FluentBundle !Send，无法进 static）。
pub static LANG_VERSION: AtomStatic<u64> = AtomStatic::new(|| 0);
pub static WORKFLOW_SNAPSHOT: AtomStatic<Option<WorkflowSnapshot>> = AtomStatic::new(|| None);

/// ACP 服务器下发的可用 slash 命令投影列表（含 skills）。
/// 由 kit notifier 在收到 `SessionUpdate::AvailableCommandsUpdate` 后写入：
/// 结构化为投影 DTO，kind/level/args 等元数据随 `_meta` 通道携带，解析在
/// acp_notifier `available_commands_update` 分支完成（`_meta` 五键 +
/// 缺省回退 kind=Command/level=1），写入后立即 `refresh_slash_items()`。
pub static AVAILABLE_SLASH_COMMANDS: AtomStatic<Vec<SlashCommandEntry>> = AtomStatic::new(Vec::new);
pub static WIZARD_ACTIVE: AtomStatic<bool> = AtomStatic::new(|| false);
/// Setup Wizard 全量状态（步骤、Provider 列表、光标位置等）
pub static SETUP_WIZARD: AtomStatic<SetupWizardState> = AtomStatic::new(SetupWizardState::default);
pub static PREDICTION: AtomStatic<PredictionState> = AtomStatic::new(PredictionState::default);
pub static INPUT_AREA_ESC_PREFIX: AtomStatic<bool> = AtomStatic::new(|| false);

/// 最近一次复制到剪贴板的字符数（用于状态栏提示 "已复制 N 字符"）
pub static COPY_CHAR_COUNT: AtomStatic<usize> = AtomStatic::new(|| 0);
/// 复制提示显示截止时间
pub static COPY_MESSAGE_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);

/// keepgoing 按钮防抖截止时间——点击后短时间内禁用按钮，避免连续误触
/// 触发多轮空跑。超过此刻后恢复可点击。
pub static KEEPGOING_BLOCKED_UNTIL: AtomStatic<Option<Instant>> = AtomStatic::new(|| None);

/// @image 行 hover 状态（T4 §4.4）：Moved 事件命中变化时由消息区 handler
/// 写入（写入自动唤醒订阅者重渲染），渲染 body 读取决定 meta 行是否显示
/// 绝对路径 + accent 高亮；移出/遮挡 → None 恢复默认渲染。仅当「命中集合
/// 变化」时写入——防高频 Moved 风暴（§4.6）。
pub(crate) static IMAGE_HOVER: AtomStatic<Option<crate::kit::message_area::ImageHoverState>> =
    AtomStatic::new(|| None);

/// 图片预览状态机（image-p0-p1-spec §7.3 T7）：写入边界为事件/effect 与后台
/// 解码线程，渲染 body 只读（TUI-RENDER-001）。默认 `Idle`（无预览）。
#[derive(Debug, Clone, Default)]
pub enum ImagePreviewState {
    #[default]
    Idle,
    /// 后台解码中（T5 全链校验已通过；`grade` 为分级结果）。
    Loading { path: PathBuf, grade: PathGrade },
    /// 解码完成：像素数据以 `Arc` 共享给 overlay 渲染（同路径重复请求复用）。
    Ready {
        path: PathBuf,
        meta: ImageMeta,
        img: Arc<DynamicImage>,
    },
    /// 手工路径/非受管理目录（§6.1 Q6）：仅文本 meta 行降级，不触发解码。
    Degraded { path: PathBuf, reason: String },
    /// 校验/解码失败：固定错误文案（安全降级，reason 仅内部诊断，不含路径）。
    Error { path: PathBuf, reason: String },
}

pub static IMAGE_PREVIEW_STATE: AtomStatic<ImagePreviewState> =
    AtomStatic::new(ImagePreviewState::default);

/// 输入区快照（§7.3 cursor 触发源）：input_area 渲染 body 每帧
/// `write_no_update` 写入（同 keepgoing_rect 模式，TUI-RENDER-001 派生缓存），
/// overlay 组件经渲染循环读取最新值。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputSnapshot {
    /// 输入区当前全文。
    pub text: String,
    /// 光标位置（字符索引，非字节）。
    pub cursor_char: usize,
}

pub static INPUT_SNAPSHOT: AtomStatic<InputSnapshot> = AtomStatic::new(InputSnapshot::default);

/// 渲染心跳计数器——后台任务每 5 秒 +1，确保 render loop 周期性唤醒。
/// 即使终端无输入、atom 无变化，也能防止 `futures::select` 在 EventStream 阻塞时
/// 永久卡死。AppShell 组件 `use_atom` 订阅此 atom。
pub static RENDER_HEARTBEAT: AtomStatic<u64> = AtomStatic::new(|| 0);

/// 当前活跃 session 的 ID。由 submit_consumer/thread_load_consumer 在 session 变更时设置。
/// acp_bridge 在 reset 后用于过滤陈旧事件（event.active_session_id != ACTIVE_SESSION_ID → 丢弃）。
pub static ACTIVE_SESSION_ID: AtomStatic<String> = AtomStatic::new(String::new);

/// 当前活跃 session 的标题。由 service_snapshot 周期性从 thread_store 派生
/// （load_meta(ACTIVE_SESSION_ID)），InputArea 上边栏右侧以 hash 稳定底色展示。
/// 空字符串表示尚无标题（新会话 / 未加载），此时不渲染。
pub static CURRENT_SESSION_TITLE: AtomStatic<String> = AtomStatic::new(String::new);

/// Bridge 重置计数器——/clear 或 thread 切换时 +1，acp_bridge 检测到变更时
/// 清空 committed / has_view_commit / current_turn，防止旧 session 的 VM
/// 残留污染新 session 的消息区。
///
/// ACP server 在 session/new 响应后推送空 ViewCommit 清空旧 session 残留。
/// bridge 只需在 counter 变更时重置内部状态——新 session 的空 ViewCommit
/// 会通过正常事件流到达，确保 committed 清空。
pub static BRIDGE_RESET_COUNTER: AtomStatic<u64> = AtomStatic::new(|| 0);

/// 跨 session/load replay 存活的 compact 完成提示（manual /compact 的
/// CommandFeedback UiOnly 消息；replay 的 BRIDGE_RESET_COUNTER 重置会清空
/// committed（含已注入的 SystemNote），bridge reset 分支据此重建到
/// current_turn 后消费清空——机制沿袭 aecc2834（issue
/// 2026-08-08-e2e-compact-command-screenshot-too-early），Phase 5 Step 7
/// 文案移交 CommandFeedback 后由 handle_command_feedback 写入）。None =
/// 无待重建提示。
pub static PENDING_COMPACT_NOTE: AtomStatic<Option<String>> = AtomStatic::new(|| None);

/// TUI 内部事件通道——input_area 本地提交通过此 channel 发送 LocalUserBubble
/// 到 acp_bridge，统一走 dispatch_and_notify 路径写入 VIEW_MODELS atom。
pub static LOCAL_EVENT_TX: OnceLock<UnboundedSender<AcpEventWithEpoch>> = OnceLock::new();

/// Spinner token 计数——由 acp_bridge 在收到 TokenUsage 事件时写入（input+output），
/// MessageArea 的 build_footer_lines 读取后作为参数传入 `render_to_lines(..., token_count)`，
/// 最终在 spinner 行右侧显示 `↓ X.Xk tokens`。render body 纯只读，不写 spinner state。
pub static SPINNER_TOKEN_COUNT: AtomStatic<usize> = AtomStatic::new(|| 0);

/// 上下文窗口使用率信息：(pct 0.0-100.0, context_total_tokens)。
/// 由 acp_notifier 在收到 StateSnapshotMeta 时从 budget_pct 写入。
/// StatusBarRow1 订阅此 atom 显示。
pub static CONTEXT_USAGE: AtomStatic<Option<(f64, u64)>> = AtomStatic::new(|| None);

/// 最近一次消息区视口快照。由 MessageArea 在 render 阶段计算后写入，
/// 仅供调试导出命令读取；screen 模式按此范围导出当前可见文本。
#[derive(Debug, Clone, Default)]
pub struct MessageViewportSnapshot {
    pub scroll_y: u16,
    pub vis_height: u16,
    pub first_line: usize,
    pub last_line: usize,
}
static MESSAGE_VIEWPORT: OnceLock<parking_lot::RwLock<MessageViewportSnapshot>> = OnceLock::new();

pub fn message_viewport_snapshot() -> &'static parking_lot::RwLock<MessageViewportSnapshot> {
    MESSAGE_VIEWPORT.get_or_init(|| parking_lot::RwLock::new(MessageViewportSnapshot::default()))
}

// ── Background Tasks 相关 State ──────────────────────────────────────────────

/// 后台任务条目列表（TUI 侧定义，与 agent 层 BgTaskInfo 对应）
pub use crate::kit::acp_types::BgTaskEntry;

/// 活跃的后台任务列表（由 bg-task-started/completed/cancelled 事件维护）
pub static BG_TASKS: AtomStatic<Vec<BgTaskEntry>> = AtomStatic::new(Vec::new);

// ── Background Display Area (后台显示区域) ────────────────────────────────────

/// 后台显示区域条目（由 bg-task-* + subagent tool 事件维护）
#[derive(Debug, Clone)]
pub struct BgDisplayEntry {
    /// 唯一标识：task_id 或 agent_id
    pub id: String,
    /// 任务类型标签："coder" / "explorer" / "bg-shell" / "workflow"
    pub agent_type: String,
    /// 任务描述（来自 BgTaskEntry.summary）
    pub desc: String,
    /// 当前执行的工具名（None 为空闲态）
    pub current_tool: Option<String>,
    /// 已完成工具调用计数
    pub tool_count: u32,
    /// false → 3s 倒计时中，到期后渲染层移除
    pub is_active: bool,
    /// 失败标志
    pub is_error: bool,
    /// 创建时间（用于显示运行时长）
    pub created_at: Instant,
    /// 完成时间（3s 倒计时起点）
    pub completed_at: Option<Instant>,
}

/// 后台显示区域条目列表（仅活跃 + 3s 缓冲中的任务）
pub static BG_DISPLAY: AtomStatic<Vec<BgDisplayEntry>> = AtomStatic::new(Vec::new);

/// 后台 agent_id 集合——用于判断 tool 事件是否属于后台任务
/// key = SubagentStarted.instance_id (is_background=true)
pub static BG_AGENT_IDS: AtomStatic<std::collections::HashSet<String>> =
    AtomStatic::new(std::collections::HashSet::new);

/// 通知消息（状态栏短暂显示，过期后自动忽略）
pub struct Notification {
    pub message: String,
    pub until: Instant,
}
pub static NOTIFICATION: AtomStatic<Option<Notification>> = AtomStatic::new(|| None);

// ── Confirm Popup 相关 State ─────────────────────────────────────────────────

/// 确认弹窗要执行的操作
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// 切换到指定 thread_id
    ThreadSwitch(String),
    /// 用户确认拒绝回答 AskUser 提问
    RejectAskUser,
}

/// 确认弹窗的 payload
#[derive(Debug, Clone)]
pub struct ConfirmPayload {
    pub title: String,
    pub message: String,
    pub details: Vec<String>,
    pub pending_action: ConfirmAction,
}

pub static CONFIRM_PAYLOAD: AtomStatic<Option<ConfirmPayload>> = AtomStatic::new(|| None);

// ── Download Progress Popup 相关 ────────────────────────────────────────────

/// 下载进度中的单文件状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDownloadStatus {
    /// 等待下载
    Pending,
    /// 正在下载
    Downloading,
    /// 下载完成
    Done,
    /// 下载失败（包含错误信息）
    Failed(String),
}

/// 下载进度条目
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    pub filename: String,
    pub status: FileDownloadStatus,
}

/// 下载进度弹窗的完整状态
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DownloadProgressPayload {
    pub items: Vec<DownloadItem>,
    /// 下载是否已完成（true 时 Esc 可关闭弹窗）
    pub finished: bool,
    /// 成功下载的文件数量
    pub success_count: usize,
    /// 失败的文件数量
    pub fail_count: usize,
}

pub static DOWNLOAD_PROGRESS: AtomStatic<DownloadProgressPayload> =
    AtomStatic::new(DownloadProgressPayload::default);

pub fn init_atoms() {}
