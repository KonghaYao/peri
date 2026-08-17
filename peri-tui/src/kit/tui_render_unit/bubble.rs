use super::fold::{EntryStatus, FoldState, entry_status_code, fold_state_code};
use super::hash::{tui_hash_combine, tui_hash_roll, tui_hash_str};
use super::reminder::{ReminderInfo, detect_reminder};

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
