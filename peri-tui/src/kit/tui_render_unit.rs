//! TuiRenderUnit —— TUI 内部渲染单元类型，不共享给 ACP 层。

use crate::i18n;
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Hash 辅助函数
// ---------------------------------------------------------------------------

/// 内容哈希——rebuild 时用于检测是否需重新渲染。
pub fn tui_hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 滚动哈希的乘法因子（奇数，保证乘法可逆，避免信息丢失）。
const HASH_ROLL_MUL: u64 = 0x9E37_79B9_7F4A_7C15;
/// 组合哈希的乘法因子——与滚动因子区分，降低结构相关性。
const HASH_COMBINE_MUL: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// 对文本按字节做滚动哈希。
///
/// 分块无关：`tui_hash_roll("ab") == tui_hash_roll_update(tui_hash_roll_update(0, "a"), "b")`，
/// 因此流式追加时增量维护与一次性全量计算产出相同值——相同内容必然产生相同 hash，
/// 且增量路径不需要保留 chunk 边界历史。
pub fn tui_hash_roll(text: &str) -> u64 {
    let mut h: u64 = 0;
    for &b in text.as_bytes() {
        h = h.wrapping_mul(HASH_ROLL_MUL).wrapping_add(u64::from(b));
    }
    h
}

/// 滚动哈希的增量更新：在已有滚动值 `h` 上追加 `chunk` 的字节。
pub fn tui_hash_roll_update(mut h: u64, chunk: &str) -> u64 {
    for &b in chunk.as_bytes() {
        h = h.wrapping_mul(HASH_ROLL_MUL).wrapping_add(u64::from(b));
    }
    h
}

/// 将两个 u64 哈希值确定性地组合为一个（内容敏感）。
pub fn tui_hash_combine(h: u64, x: u64) -> u64 {
    h.wrapping_mul(HASH_COMBINE_MUL).wrapping_add(x)
}

// ---------------------------------------------------------------------------
// PartialEq 辅助宏——跳过 content_hash 字段
// ---------------------------------------------------------------------------

/// Implement `PartialEq` for a struct, comparing only the listed fields
/// (excluding `content_hash`).
macro_rules! tui_impl_partial_eq {
    ($ty:ty: $($field:ident),+ $(,)?) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Top-level enum
// ---------------------------------------------------------------------------

/// Discriminated-union TuiRenderUnit consumed by the TUI renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiRenderUnit {
    TuiUserBubble(TuiUserBubble),
    TuiAssistantBubble(TuiAssistantBubble),
    TuiToolCard(TuiToolCard),
    TuiSystemNote(TuiSystemNote),
    TuiSubAgentGroup(TuiSubAgentGroup),
    TuiCollapsedGroup(TuiCollapsedGroup),
    TuiDivider(TuiDivider),
    TuiAskUserBlock(TuiAskUserBlock),
    /// §6.9 活动 turn 的 todo 进度摘要行（`3/7 tasks · Running tests`），
    /// 由 push_view_models 从 `TODO_ITEMS` 派生，插在最终回答之前。
    TuiTodoSummary(TuiTodoSummary),
}

impl TuiRenderUnit {
    /// 返回该 VM 内部存储的 content_hash。
    /// 供按 VM 分片的渲染缓存作为 key 使用——hash 不变时直接 Arc::clone 复用渲染结果。
    pub fn content_hash(&self) -> u64 {
        match self {
            Self::TuiUserBubble(d) => d.content_hash,
            Self::TuiAssistantBubble(d) => d.content_hash,
            Self::TuiToolCard(d) => d.content_hash,
            Self::TuiSystemNote(d) => d.content_hash,
            Self::TuiSubAgentGroup(d) => d.content_hash,
            Self::TuiCollapsedGroup(d) => d.content_hash,
            Self::TuiDivider(d) => d.content_hash,
            Self::TuiAskUserBlock(d) => d.content_hash,
            Self::TuiTodoSummary(d) => d.content_hash,
        }
    }
}

// ---------------------------------------------------------------------------
// 折叠状态机（spec §7）——折叠策略的**唯一**定义点
// ---------------------------------------------------------------------------

/// 折叠三态（spec §7）——`Collapsed` 单行 / `Preview` 有界 tail / `Expanded` 完整 body。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldState {
    #[default]
    Collapsed,
    Preview,
    Expanded,
}

/// Entry 生命周期状态——折叠表按此选择默认 fold。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryStatus {
    Running,
    #[default]
    Completed,
    Error,
}

/// §7 折叠表的 entry 类型维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldTarget {
    User,
    Assistant,
    Reasoning,
    Tool,
    SubAgent,
    System,
    Interaction,
}

/// 折叠覆盖键——用户手动操作过的 entry 身份（spec §7「用户手动改变 fold state
/// 后，本 turn 内不再被自动策略覆盖」）。按 ACP 身份字段键控：
/// `Reasoning(message_id)` / `Tool(tool_id)` / `SubAgent(agent_id)` /
/// `Interaction(request_id)`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldKey {
    Reasoning(String),
    Tool(String),
    SubAgent(String),
    /// Interaction block 按本地 request_id 键控（生产创建点从
    /// HITL_REQUEST_ID / ASK_USER_REQUEST_ID atom 克隆；测试构造为 None 时
    /// `fold_key_of` 返回 None——与 reasoning 的 message_id 先例一致）。
    Interaction(String),
}

/// [G2] spec §7 折叠表——每个 entry 类型 × 状态的默认折叠目标。
///
/// 唯一折叠策略单点：`push_view_models` 的折叠 pass（以及未来所有消费者）
/// 只能从这里取值，禁止在别处内联折叠决策。
pub fn fold_for_status(target: FoldTarget, status: EntryStatus) -> FoldState {
    use EntryStatus::*;
    use FoldTarget::*;
    match (target, status) {
        // user / assistant 正文永远展开（user 长文折叠归 Slice 3 截断层）
        (User, _) | (Assistant, _) => FoldState::Expanded,
        // reasoning：running = tail preview，completed 自动收束为单行
        (Reasoning, Running) | (Reasoning, Error) => FoldState::Preview,
        (Reasoning, Completed) => FoldState::Collapsed,
        // tool：running = tail preview，success 默认折叠，error 展开错误摘要
        (Tool, Running) => FoldState::Preview,
        (Tool, Completed) => FoldState::Collapsed,
        (Tool, Error) => FoldState::Expanded,
        // subagent：running = Collapsed + live summary（裁决 C4：按 spec §7 表）
        (SubAgent, Running) | (SubAgent, Completed) => FoldState::Collapsed,
        (SubAgent, Error) => FoldState::Expanded,
        // system：普通事件单行 divider，error 展开摘要
        (System, Running) | (System, Completed) => FoldState::Collapsed,
        (System, Error) => FoldState::Expanded,
        // interaction：等待时 expanded 可聚焦，答毕收束为结果行
        (Interaction, Running) => FoldState::Expanded,
        (Interaction, Completed) => FoldState::Collapsed,
        (Interaction, Error) => FoldState::Expanded,
    }
}

/// `FoldState` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
pub fn fold_state_code(f: FoldState) -> u64 {
    match f {
        FoldState::Collapsed => 1,
        FoldState::Preview => 2,
        FoldState::Expanded => 3,
    }
}

/// `EntryStatus` 的确定性 hash 代码——纳入 content_hash 公式（G1）。
pub fn entry_status_code(s: EntryStatus) -> u64 {
    match s {
        EntryStatus::Running => 1,
        EntryStatus::Completed => 2,
        EntryStatus::Error => 3,
    }
}

// ---------------------------------------------------------------------------
// Leaf data structures
// ---------------------------------------------------------------------------

/// System-reminder 分类——10 种从 `<system-reminder>` 标签检测到的类型。
#[derive(Debug, Clone, PartialEq)]
pub enum ReminderType {
    /// Channel（微信/Slack/飞书等）来源消息
    ChannelMessage(String),
    /// Cron 定时任务注入
    CronReminder,
    /// 后台任务完成通知
    BgTaskCompleted,
    /// Fork 模式背景 Agent 注入
    ForkMode,
    /// 上下文压缩摘要
    ContextCompacted,
    /// CONTINUATION_HINT 系统提示
    ContinuationHint,
    /// 信任边界声明
    TrustBoundary,
    /// 工具相关系统提醒
    ToolReminder,
    /// 子 Agent 结果摘要
    SubagentResult,
    /// 未匹配分类的兜底类型
    GenericReminder,
}

impl ReminderType {
    /// 中文标签，用于缩略渲染第一行。
    /// 返回 `String` 而非 `&'static str` 是为了 `ChannelMessage` 的动态 source。
    pub fn label(&self) -> String {
        match self {
            ReminderType::ChannelMessage(source) => format!("Channel ({})", source),
            ReminderType::CronReminder => i18n::tr("reminder-cron-task"),
            ReminderType::BgTaskCompleted => i18n::tr("reminder-bg-task"),
            ReminderType::ForkMode => i18n::tr("reminder-fork-mode"),
            ReminderType::ContextCompacted => i18n::tr("reminder-context-compaction"),
            ReminderType::ContinuationHint => i18n::tr("reminder-system-prompt"),
            ReminderType::TrustBoundary => i18n::tr("reminder-trust-boundary"),
            ReminderType::ToolReminder => i18n::tr("reminder-tool-reminder"),
            ReminderType::SubagentResult => i18n::tr("reminder-subagent-result"),
            ReminderType::GenericReminder => i18n::tr("reminder-system-reminder"),
        }
    }
}

/// 从 `<system-reminder>` 标签解析的信息——类型 + 摘要文本。
#[derive(Debug, Clone, PartialEq)]
pub struct ReminderInfo {
    pub reminder_type: ReminderType,
    /// 首非空行数据摘要，截断到 200 字符
    pub summary: String,
}

/// User message bubble -- right-aligned plain text.
#[derive(Debug, Clone)]
pub struct TuiUserBubble {
    pub text: String,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// 从文本中检测到的 system-reminder 信息。
    /// `None` 表示普通用户消息气泡。
    pub reminder: Option<ReminderInfo>,
    /// 来源标记（§6.1 来源型消息 / §10 interjection 预留，G-Interjection）。
    /// 协议无来源字段（零改动约束），生产构造点恒 `None`（普通提交）；
    /// 渲染时 `Some` 才会在 label 追加 muted 来源。身份字段，不进 content_hash
    /// （同 `message_id` 先例），进 partial_eq。
    pub source: Option<String>,
}

impl TuiUserBubble {
    /// 构造函数——自动从文本中检测 `<system-reminder>` 标签并提取
    /// [`ReminderInfo`]；`source` 填充占位 `None`（协议无来源标记）。
    pub fn new(text: String) -> Self {
        let content_hash = tui_hash_str(&text);
        let reminder = detect_reminder(&text);
        TuiUserBubble {
            text,
            content_hash,
            reminder,
            source: None,
        }
    }
}

tui_impl_partial_eq!(TuiUserBubble: text, reminder, source);

/// Agent reply bubble -- left-aligned markdown with optional reasoning block.
///
/// Tool invocations are **siblings** (separate `TuiToolCard` entries), not
/// embedded inside the bubble.
#[derive(Debug, Clone)]
pub struct TuiAssistantBubble {
    /// Markdown source text.
    pub text: String,
    /// Optional reasoning / thinking block (Anthropic extended thinking etc.).
    pub reasoning: Option<TuiReasoningBlock>,
    /// 身份字段——ACP `messageId`，折叠覆盖键 `FoldKey::Reasoning(message_id)` 用。
    /// [G1] 不进 content_hash（同一消息内容变化不应因 id 失效），进 partial_eq。
    pub message_id: Option<String>,
    /// 本 bubble 文本流式开始的时刻（§6.2 `12.4s`）——仅 trailing 流式段有值；
    /// 折叠 pass 在 phase 离开 PromptRunning 时冻结为 `duration_ms`（镜像
    /// reasoning 的冻结机制），冻结后置 None。身份/时序字段，进 partial_eq。
    pub started_at: Option<std::time::Instant>,
    /// 已冻结的正文时长（毫秒）——仅完成后的 bubble 有值（折叠 pass 冻结，
    /// 此后不再增长）；Running 中为 None。进 partial_eq。
    pub duration_ms: Option<u64>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiAssistantBubble {
    /// 计算包含以下内容的 hash：text、reasoning.text、
    /// reasoning.fold/status/is_running/duration、
    /// 正文时长（`duration_secs`，None→0，秒取整）、冻结判别位（`frozen`）。
    ///
    /// sync_cache 的增量路径（`build_bubble_parts`）与 push_view_models 的折叠
    /// pass 都用同一公式，保证修改折叠状态后 hash 一致（G1 三单点）。
    ///
    /// `frozen`（= `started_at.is_none()`）：running→frozen 翻转在同一秒内落地
    /// 时 `duration_secs` 数值可能不变，但渲染内容不同（§6.2 `12.4s` meta 从
    /// 无到有）——hash 必须区分，否则按 hash 分片的渲染缓存持续供应运行中
    /// （无 meta）的旧帧（回归：冻结翻转后 duration meta 缺失/闪烁）。
    ///
    /// 文本部分使用滚动哈希（[`tui_hash_roll`]）——流式追加时可由增量维护的
    /// 滚动值直接组合，避免每 token 对全量文本 format! + 哈希。
    /// `message_id` 是身份字段，不参与 hash。
    pub fn compute_hash(
        text: &str,
        reasoning: Option<&TuiReasoningBlock>,
        duration_secs: u64,
        frozen: bool,
    ) -> u64 {
        let mut h = tui_hash_roll(text);
        if let Some(r) = reasoning {
            h = tui_hash_combine(h, tui_hash_roll(&r.text));
            h = tui_hash_combine(h, fold_state_code(r.fold));
            h = tui_hash_combine(h, entry_status_code(r.status));
            h = tui_hash_combine(h, u64::from(r.is_running));
            h = tui_hash_combine(h, r.duration_code());
        }
        h = tui_hash_combine(h, duration_secs);
        tui_hash_combine(h, u64::from(frozen))
    }

    /// [G1] 正文时长对 hash 的确定性贡献：Running 按已耗时秒数（随时间变化，
    /// 触发按秒重建），Completed 按冻结秒数（稳定）。与 `TuiReasoningBlock::duration_code`
    /// 同语义。
    pub fn duration_secs(&self) -> u64 {
        if let Some(started) = self.started_at {
            started.elapsed().as_secs()
        } else {
            self.duration_ms.unwrap_or(0) / 1000
        }
    }

    /// 根据 text + reasoning + duration 当前值重算 content_hash。
    /// 修改 reasoning.fold/status 或冻结 duration 后必须调用，否则按 hash 分片的
    /// 渲染缓存会命中旧值。
    pub fn recompute_hash(&mut self) {
        self.content_hash = Self::compute_hash(
            &self.text,
            self.reasoning.as_ref(),
            self.duration_secs(),
            // [G1] 冻结判别位 = started_at 是否已清除（running 形态 ↔ 冻结形态）。
            self.started_at.is_none(),
        );
    }

    /// [LOW-5] 稳定身份 hash——排除时变 duration（正文/推理的秒数），供
    /// 复制按钮点击校验使用：运行中 bubble 的 content_hash 每秒随 duration
    /// 漂移，渲染帧与点击事件跨秒边界时按 content_hash 比对会偶发拒绝命中
    /// （下一帧自愈，但点击丢失）。身份校验只关心文本与折叠/状态；
    /// `message_id` 身份字段同样排除（与 compute_hash 口径一致）。
    pub fn stable_identity_hash(text: &str, reasoning: Option<&TuiReasoningBlock>) -> u64 {
        let mut h = tui_hash_roll(text);
        if let Some(r) = reasoning {
            h = tui_hash_combine(h, tui_hash_roll(&r.text));
            h = tui_hash_combine(h, fold_state_code(r.fold));
            h = tui_hash_combine(h, entry_status_code(r.status));
            h = tui_hash_combine(h, u64::from(r.is_running));
            // 不含 duration_code——时变，不属于身份。
        }
        h
    }
}

tui_impl_partial_eq!(TuiAssistantBubble: text, reasoning, message_id, started_at, duration_ms);

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TuiToolPresentation {
    #[default]
    Generic,
    Skill(TuiSkillPresentation),
    Todo(TuiTodoPresentation),
}

/// User-facing information for a successfully loaded skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiSkillPresentation {
    pub name: String,
}

/// User-facing TodoWrite snapshot and the changes from its previous successful call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoPresentation {
    pub current_items: Vec<TuiTodoItem>,
    pub changes: Vec<TuiTodoChange>,
    pub is_initial: bool,
    pub completed_count: usize,
    pub total_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoItem {
    pub content: String,
    pub active_form: Option<String>,
    pub status: TuiTodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTodoChange {
    pub kind: TuiTodoChangeKind,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTodoChangeKind {
    Added,
    Started,
    Completed,
    Reopened,
    ActiveFormUpdated,
    Removed,
}

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone)]
pub struct TuiToolCard {
    /// Stable identifier for this tool call.
    pub tool_id: String,
    /// Human-readable tool name (e.g. "Edit", "Bash").
    pub tool_name: String,
    /// One-line summary of the input / arguments.
    pub input_summary: String,
    /// One-line summary of the output / result.
    pub output_summary: String,
    /// Whether the tool invocation resulted in an error.
    pub is_error: bool,
    /// Whether the tool is still streaming/running.
    pub is_running: bool,
    /// Elapsed time in milliseconds for a running tool.
    pub running_duration_ms: Option<u64>,
    /// 已完成的工具冻结时长（毫秒）——`end_tool` 时由同源 `started_at` 冻结
    /// （G-started_at），完成行 `37ms`/`4.2s` 显示用；Running 中为 `None`。
    pub completed_duration_ms: Option<u64>,
    /// Inline diff preview (Write / Edit tools).
    pub diff: Option<TuiDiffBlock>,
    /// 专属工具的用户语义展示；默认保持通用工具卡片。
    pub presentation: TuiToolPresentation,
    /// 折叠状态——折叠 pass（spec §7 表）与用户覆盖（FOLD_OVERRIDES）驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// Agent 工具专用的子工具调用计数（由 sync_cache 后处理 pair_agent_tool_cards 配对填充）。
    pub tool_calls_count: usize,
}

impl TuiToolCard {
    /// [G1] 内容哈希公式单点——`build_tool_card` / replay 构造 / 折叠 pass 共用。
    /// 包含 fold + user_modified：折叠状态变化必须触发按 hash 分片的渲染缓存重建。
    /// duration 按秒取整后纳入 hash——避免每毫秒 hash 变化导致分片缓存频繁失效；
    /// 同时保证 duration 文本每秒刷新。completed_duration_ms 是冻结值（秒级稳定）。
    pub fn recompute_hash(&mut self) {
        let duration_secs = self.running_duration_ms.map(|ms| ms / 1000);
        let completed_secs = self.completed_duration_ms.map(|ms| ms / 1000);
        let mut h = tui_hash_str(&format!(
            "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
            self.tool_id,
            self.tool_name,
            self.input_summary,
            self.output_summary,
            self.is_error,
            self.is_running,
            duration_secs,
            completed_secs,
            self.presentation,
            self.fold,
            self.user_modified,
        ));
        // [G-Diff] diff 定型于 tool-ended，此后不变——稳定摘要纳入 hash 保证
        // diff 变更（含路径/计数/截断）触发按 hash 分片的渲染缓存重建。
        h = tui_hash_combine(h, self.diff_code());
        self.content_hash = h;
    }

    /// [G-Diff] diff 的稳定摘要 hash：path + 总 change 数 + is_binary/is_too_large/
    /// is_new_file + 截断信息。`None` → 0（普通工具卡无 diff 不改变 hash）。
    pub fn diff_code(&self) -> u64 {
        let Some(d) = &self.diff else {
            return 0;
        };
        let (adds, dels) = diff_change_counts(d);
        let mut h = tui_hash_combine(0, tui_hash_str(&d.path));
        h = tui_hash_combine(h, adds as u64);
        h = tui_hash_combine(h, dels as u64);
        h = tui_hash_combine(h, d.more_change_lines as u64);
        h = tui_hash_combine(h, u64::from(d.is_binary));
        h = tui_hash_combine(h, u64::from(d.is_too_large));
        h = tui_hash_combine(h, u64::from(d.is_new_file));
        h
    }
}

tui_impl_partial_eq!(TuiToolCard: tool_id, tool_name, input_summary, output_summary, is_error, is_running, running_duration_ms, completed_duration_ms, diff, presentation, fold, user_modified);

/// System notification -- centered banner for model switches, compact, etc.
#[derive(Debug, Clone)]
pub struct TuiSystemNote {
    pub text: String,
    pub level: TuiNoteLevel,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiSystemNote: text, level);

/// Severity of a system note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiNoteLevel {
    Info,
    Warning,
    Error,
}

/// Sub-agent message group -- bounded by start/stop events.
///
/// Nested `view_models` render inside a collapsible container.
#[derive(Debug, Clone)]
pub struct TuiSubAgentGroup {
    pub agent_id: String,
    pub agent_name: String,
    /// Nested view models produced by the sub-agent.
    pub view_models: im::Vector<TuiRenderUnit>,
    /// Whether the group is currently collapsed（详情面板语义；消息区折叠由 fold 驱动）。
    pub collapsed: bool,
    /// Whether the sub-agent is still streaming.
    pub is_running: bool,
    /// 折叠状态——折叠 pass（spec §7 表）与用户覆盖（FOLD_OVERRIDES）驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiSubAgentGroup {
    /// [G1] 内容哈希公式单点——`SubAgentAccumulator::view_model` 构造与折叠 pass
    /// 共用（含 fold + user_modified：状态变化必须触发分片渲染缓存重建）。
    pub fn recompute_hash(&mut self) {
        // 用 u64 组合累加每个 child VM 的 content_hash，确保 child 文本
        // 变化时（即使 view_models.len() 不变）也能触发按 hash 分片的渲染缓存重建。
        let mut child_hash_total: u64 = 0;
        for vm in self.view_models.iter() {
            child_hash_total = tui_hash_combine(child_hash_total, vm.content_hash());
        }
        let mut h = tui_hash_combine(0, tui_hash_str(&self.agent_id));
        h = tui_hash_combine(h, tui_hash_str(&self.agent_name));
        h = tui_hash_combine(h, self.view_models.len() as u64);
        h = tui_hash_combine(h, 0); // collapsed 恒为 false（详情面板保持展开；消息区折叠由 fold 驱动）
        h = tui_hash_combine(h, u64::from(self.is_running));
        h = tui_hash_combine(h, fold_state_code(self.fold));
        h = tui_hash_combine(h, u64::from(self.user_modified));
        h = tui_hash_combine(h, child_hash_total);
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiSubAgentGroup: agent_id, agent_name, view_models, collapsed, is_running, fold, user_modified);

/// Generic collapsible group -- e.g. batched tool calls.
#[derive(Debug, Clone)]
pub struct TuiCollapsedGroup {
    pub title: String,
    /// Number of items hidden when collapsed.
    pub count: u32,
    /// 组后**连续相邻**的 error 工具数（D2：error 不入组、不删除、保持展开，
    /// 标题追加 `· N failed`）。由 `group_successful_tools` 从 run 结束位置
    /// 向后扫描连续相邻 error `TuiToolCard` 计入。
    pub failed_count: u32,
    /// The view models inside the group (visible when expanded).
    pub view_models: Vec<TuiRenderUnit>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiCollapsedGroup {
    /// [G1] 内容哈希公式单点——`group_successful_tools` 构造时调用。
    /// 成员变化（标题/数量/失败数/隐藏 VM）必须触发按 hash 分片的渲染缓存重建。
    pub fn recompute_hash(&mut self) {
        let mut child_hash_total: u64 = 0;
        for vm in self.view_models.iter() {
            child_hash_total = tui_hash_combine(child_hash_total, vm.content_hash());
        }
        let mut h = tui_hash_combine(0, tui_hash_str(&self.title));
        h = tui_hash_combine(h, u64::from(self.count));
        h = tui_hash_combine(h, u64::from(self.failed_count));
        h = tui_hash_combine(h, self.view_models.len() as u64);
        h = tui_hash_combine(h, child_hash_total);
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiCollapsedGroup: title, count, failed_count, view_models);

/// Visual separator between iteration rounds.
#[derive(Debug, Clone)]
pub struct TuiDivider {
    /// Optional label rendered next to the line (e.g. "Round 3").
    pub label: Option<String>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiDivider: label);

/// AskUser question-answer block — rendered after user responds to AskUserQuestion tool.
///
/// Slice 4（§6.8）双轨落地：production 创建点（`handle_ask_user` /
/// `handle_hitl_pending`）push 到 `state.committed`（不进 CurrentTurn 缓存），
/// 承担「可见 + 可聚焦 + 结果回写」；AskUser 面板 / HITL 弹窗保留为模态操作层
/// （D5）。历史 items 字段保留（问答对渲染兼容旧数据），新路径以
/// kind/pending/verb/question/options/result 为准。
#[derive(Debug, Clone)]
pub struct TuiAskUserBlock {
    /// Question-answer pairs extracted from tool input/output（历史字段）。
    pub items: Vec<TuiAskUserItem>,
    /// Whether any item indicates an error response.
    pub is_error: bool,
    /// 交互类型：Permission（HITL）或 AskUser 表单（§6.8）。
    pub kind: InteractionKind,
    /// 是否仍在等待用户响应。pending → 折叠表 Running（Expanded 可聚焦）；
    /// 结果回写后 false → Completed（Collapsed 结果行）。
    pub pending: bool,
    /// 动作动词（如 `Bash`；AskUser 恒 `AskUser`）。
    pub verb: String,
    /// 人类可读摘要（Permission：`Bash wants to run: cargo test`；
    /// AskUser：首问 header/options 摘要）。
    pub question: String,
    /// 可选项 label 列表（Permission：[Allow once, Deny]，D6 协议依赖；
    /// AskUser：首问 options labels）。
    pub options: Vec<String>,
    /// 提交结果（如 `Allowed once` / 用户选中 label）——仅 completed 有值；
    /// 渲染层负责加状态符号与颜色。
    pub result: Option<String>,
    /// 本地 request_id（从 HITL_REQUEST_ID / ASK_USER_REQUEST_ID atom 克隆，
    /// 即 serde_json 序列化的 RequestId 字符串）——InteractionResolved 事件
    /// 按此匹配回写；同时是折叠覆盖键 `FoldKey::Interaction(id)` 的键控。
    /// 身份字段，不进 content_hash（同 message_id/source 先例），进 partial_eq。
    pub request_id: Option<String>,
    /// 折叠状态——折叠 pass（spec §7 interaction 行）驱动；
    /// 生产创建点 push 到 committed，折叠 pass 与用户覆盖共同驱动。
    pub fold: FoldState,
    /// 用户手动操作过折叠状态——自动策略免疫（spec §7）。
    pub user_modified: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

/// §6.8 interaction block 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionKind {
    /// HITL RequestPermission 审批（`[Allow once] [Deny]`）。
    Permission,
    /// AskUser 表单（选项取首问 options labels）。
    AskUser,
}

/// [G1] InteractionKind 对 hash 的确定性贡献。
pub fn interaction_kind_code(k: &InteractionKind) -> u64 {
    match k {
        InteractionKind::Permission => 1,
        InteractionKind::AskUser => 2,
    }
}

impl TuiAskUserBlock {
    /// [G1] 内容哈希公式单点——生产创建点 / 结果回写 / 折叠 pass 共用。
    /// 包含 kind/pending/verb/question/options/result + fold/is_error/user_modified；
    /// `request_id` 是身份字段不参与（同 message_id 先例）。result 秒级稳定
    /// （提交后定型），pending 翻转与选项变化必须触发按 hash 分片的缓存重建。
    pub fn recompute_hash(&mut self) {
        let mut h = tui_hash_combine(0, interaction_kind_code(&self.kind));
        h = tui_hash_combine(h, u64::from(self.pending));
        h = tui_hash_combine(h, tui_hash_str(&self.verb));
        h = tui_hash_combine(h, tui_hash_str(&self.question));
        for opt in &self.options {
            h = tui_hash_combine(h, tui_hash_str(opt));
        }
        h = tui_hash_combine(h, self.options.len() as u64);
        h = tui_hash_combine(
            h,
            match &self.result {
                Some(r) => tui_hash_str(r),
                None => 0,
            },
        );
        h = tui_hash_combine(h, fold_state_code(self.fold));
        h = tui_hash_combine(h, u64::from(self.is_error));
        h = tui_hash_combine(h, u64::from(self.user_modified));
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiAskUserBlock: items, is_error, kind, pending, verb, question, options, result, request_id, fold, user_modified);

/// §6.9 活动 turn 的 todo 进度摘要——`3/7 tasks · Running tests`。
///
/// 由 `push_view_models` 从 `TODO_ITEMS` 派生（快照后处理，非 segment 缓存），
/// 插在当前 turn 的最终回答之前；turn 结束后随 current_turn 一起消失。
#[derive(Debug, Clone)]
pub struct TuiTodoSummary {
    /// 摘要文本（含完成数/总数与 in-progress 项内容）。
    pub text: String,
    /// 内容哈希——todo 内容变化时触发按 hash 分片的渲染缓存重建。
    pub content_hash: u64,
}

impl TuiTodoSummary {
    /// 从摘要文本构造（hash = f(text)）。
    pub fn new(text: String) -> Self {
        let content_hash = tui_hash_str(&text);
        Self { text, content_hash }
    }
}

tui_impl_partial_eq!(TuiTodoSummary: text);

/// A single question-answer pair in an AskUser block.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiAskUserItem {
    /// Question header text.
    pub header: String,
    /// User's answer text.
    pub answer: String,
}

// ---------------------------------------------------------------------------
// Shared helper types
// ---------------------------------------------------------------------------

/// Collapsible reasoning / thinking block。
///
/// 折叠三态由 [`FoldState`] 表达（spec §7）；`collapsed()` 访问器映射
/// `Collapsed → true`，供渲染层保持折叠/展开二元行为。
///
/// 时长（§6.3 `Thought for 12s`）：
/// - Running 块：`started_at = Some(t0)`，渲染层按 `elapsed()` 显示秒数；
///   hash 含按秒取整的 elapsed——流式期间时长文本随 token 重建刷新。
/// - Completed 块：`duration_ms = Some(冻结值)`（segment flush 或折叠 pass
///   在 phase 离开 PromptRunning 时冻结），hash 稳定。
#[derive(Debug, Clone, PartialEq)]
pub struct TuiReasoningBlock {
    pub text: String,
    /// 折叠状态——由折叠 pass（spec §7 表）与用户覆盖（FOLD_OVERRIDES）驱动。
    pub fold: FoldState,
    /// 生命周期状态——trailing 流式段 Running，冻结/完成后 Completed。
    pub status: EntryStatus,
    /// 是否仍在流式输出（status == Running 的冗余标志，供渲染层快速判断）。
    pub is_running: bool,
    /// 推理开始时间——仅 Running 块有值（渲染 elapsed；hash 按秒取整）。
    pub started_at: Option<std::time::Instant>,
    /// 已冻结的推理时长（毫秒）——仅 Completed 块有值（折叠 pass / segment
    /// flush 时冻结，此后不再增长）。身份/时序字段，确定性参与 hash（G1）。
    pub duration_ms: Option<u64>,
}

impl TuiReasoningBlock {
    /// 折叠二元访问器——渲染层沿用现有语义：`Collapsed → true`。
    pub fn collapsed(&self) -> bool {
        self.fold == FoldState::Collapsed
    }

    /// [G1] 推理时长对 hash 的确定性贡献：Running 按已耗时秒数（随时间变化，
    /// 触发按秒重建刷新时长文本），Completed 按冻结秒数（稳定）。
    /// Completed 且 `duration_ms` 为 None（历史恢复路径，时长不可得）→ 特殊码
    /// `u64::MAX`，与 `Some(0)` 区分——渲染层省略时长（`思考了 · N 行`），
    /// 文本与 `思考了 0 秒 · N 行` 不同，hash 必须不同（防渲染缓存陈旧帧）。
    pub fn duration_code(&self) -> u64 {
        if self.is_running {
            let ms = self
                .started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            ms / 1000
        } else {
            self.duration_ms.map(|ms| ms / 1000).unwrap_or(u64::MAX)
        }
    }

    /// 展示用时长（秒数）：Running 取当前已耗时，Completed 取冻结值。
    pub fn duration_secs(&self) -> u64 {
        if self.is_running {
            self.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0)
        } else {
            self.duration_ms.unwrap_or(0) / 1000
        }
    }
}

/// Inline diff preview (for Write / Edit tool results).
#[derive(Debug, Clone, PartialEq)]
pub struct TuiDiffBlock {
    /// File path the diff applies to.
    pub path: String,
    pub hunks: Vec<TuiHunk>,
    /// Binary file -- cannot display diff.
    pub is_binary: bool,
    /// Diff content exceeded safe size limit.
    pub is_too_large: bool,
    /// New file (Write, or Edit with empty old_string) -- cap at 6 lines.
    pub is_new_file: bool,
    /// [G-Diff] 首个 hunk 之后所有未展示 hunk 的 change（`+`/`-`）行总数——
    /// 渲染层在首个 hunk 后显示 `… +N more lines`（§6.5）。
    pub more_change_lines: usize,
    /// [G-Diff] 顶层 `+`/`-` 总计数（header `+N −M` 渲染与 hash 共用）：
    /// unified diff 时 = 全部 hunk 内 change 行数；摘要解析
    /// （`parse_edit_write_summary`，无 hunk）时 = 摘要提取的行数。
    pub adds: usize,
    pub dels: usize,
}

/// A single diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunk {
    /// Header range string for the old side (e.g. "@@ -1,3 +1,4 @@").
    pub old_range: String,
    /// Header range string for the new side.
    pub new_range: String,
    pub lines: Vec<TuiHunkLine>,
    /// [G-Diff] 本 hunk 内超出上限（§6.5「最多 8 个 change 行」）被截断的
    /// change 行数——渲染层追加 `… +N more lines`。
    pub truncated_lines: usize,
}

/// One line inside a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunkLine {
    pub kind: TuiHunkLineKind,
    /// Content text (without the leading +/- or space prefix).
    pub text: String,
    /// Line number on the old side (None for pure-add lines).
    pub old_no: Option<u32>,
    /// Line number on the new side (None for pure-delete lines).
    pub new_no: Option<u32>,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiHunkLineKind {
    /// Unchanged context line.
    Context,
    /// Added line.
    Add,
    /// Deleted line.
    Del,
}

/// [G-Diff] diff 的 change 行计数（Add/Del 总数）——
/// header `+N −M` 渲染与 [`TuiToolCard::diff_code`] hash 共用。
/// 顶层字段由构造点填充（unified 解析 = hunk 内统计；摘要解析 = 文本计数）。
pub fn diff_change_counts(diff: &TuiDiffBlock) -> (usize, usize) {
    (diff.adds, diff.dels)
}

// ---------------------------------------------------------------------------
// system-reminder 检测函数
// ---------------------------------------------------------------------------

/// 提取 `<system-reminder>` 标签间的内部文本（首个匹配）。
fn extract_reminder_inner(text: &str) -> Option<String> {
    let tag = "<system-reminder>";
    let close_tag = "</system-reminder>";
    let start = text.find(tag)?;
    let content_start = start + tag.len();
    let end = text[content_start..].find(close_tag)?;
    Some(text[content_start..content_start + end].trim().to_string())
}

/// 从 reminder 内部文本提取 channel 来源短名。
fn extract_channel_source(inner: &str) -> Option<String> {
    // 匹配 plugin:name:name 格式
    if let Some(plugin_pos) = inner.find("plugin:") {
        let after = &inner[plugin_pos + "plugin:".len()..];
        if let Some(colon_pos) = after.find(':') {
            let raw = &after[..colon_pos];
            // 映射到显示名
            let display = match raw {
                "weixin" | "wechat" => i18n::tr("channel-wechat"),
                "slack" => "Slack".to_string(),
                "feishu" => i18n::tr("channel-feishu"),
                "dingtalk" => i18n::tr("channel-dingtalk"),
                "telegram" => "Telegram".to_string(),
                other => other.to_string(),
            };
            return Some(display.to_string());
        }
    }

    // channel source 关键词直搜
    let lower = inner.to_lowercase();
    for (kw, display) in &[
        ("weixin", i18n::tr("channel-wechat")),
        ("wechat", i18n::tr("channel-wechat")),
        ("slack", "Slack".to_string()),
        ("feishu", i18n::tr("channel-feishu")),
        ("dingtalk", i18n::tr("channel-dingtalk")),
        ("telegram", "Telegram".to_string()),
    ] {
        if lower.contains(kw) {
            return Some(display.to_string());
        }
    }

    None
}

/// 按优先级分类 reminder 类型。
fn classify_reminder_type(inner: &str, _full_text: &str) -> ReminderType {
    if inner.contains("CONTINUATION_HINT") {
        return ReminderType::ContinuationHint;
    }
    if let Some(source) = extract_channel_source(inner) {
        return ReminderType::ChannelMessage(source);
    }
    let lower = inner.to_lowercase();
    if lower.contains("cron") || lower.contains("scheduled") {
        ReminderType::CronReminder
    } else if lower.contains("background") || lower.contains("bgtask") || inner.contains("后台") {
        ReminderType::BgTaskCompleted
    } else if lower.contains("fork") {
        ReminderType::ForkMode
    } else if lower.contains("compact") || inner.contains("压缩") {
        ReminderType::ContextCompacted
    } else if inner.contains("Trust boundary") || inner.contains("信任边界") {
        ReminderType::TrustBoundary
    } else if lower.contains("tool") || inner.contains("工具") {
        ReminderType::ToolReminder
    } else if lower.contains("subagent") || lower.contains("sub_agent") || inner.contains("子Agent")
    {
        ReminderType::SubagentResult
    } else {
        ReminderType::GenericReminder
    }
}

/// 从 reminder 内部文本提取摘要：首非空行，截断到 200 字符。
fn extract_summary(inner: &str) -> String {
    let first_line = inner.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.chars().count() <= 200 {
        trimmed.to_string()
    } else {
        let trunc: String = trimmed.chars().take(200).collect();
        format!("{}…", trunc)
    }
}

/// 公开入口：从用户消息文本中检测 `<system-reminder>` 标签。
/// 返回 `Some(ReminderInfo)` 若存在合法标签，否则 `None`。
pub fn detect_reminder(text: &str) -> Option<ReminderInfo> {
    let inner = extract_reminder_inner(text)?;
    let reminder_type = classify_reminder_type(&inner, text);
    let summary = extract_summary(&inner);
    Some(ReminderInfo {
        reminder_type,
        summary,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "tui_render_unit_test.rs"]
mod tests;
