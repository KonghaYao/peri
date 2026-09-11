//! Canonical state and lifecycle for one streaming turn.

mod projection;
mod streaming;
mod subagents;

use super::tool_card::{SubAgentAccumulator, ToolCardAccumulator};
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit};
use std::time::Instant;

// ---------------------------------------------------------------------------
// CurrentTurn + ToolCardAccumulator
// ---------------------------------------------------------------------------

/// Accumulated streaming data for the in-progress agent turn.
///
/// When `"view-commit"` arrives, the consumer clears this and replaces
/// the base view with the full snapshot. Rendering concatenates
/// `committed + CurrentTurn.view_models()`.
///
/// ## Segment interleaving
///
/// Agent text, tool calls, and sub-agent starts are interleaved at the protocol
/// level: the model says a few words, then calls a tool, then continues speaking.
/// `segments` records this chronological order so that `sync_cache` can
/// create separate `TuiAssistantBubble` entries for text before and after each
/// tool/sub-agent boundary, instead of merging everything into one fat bubble.
#[derive(Debug, Clone)]
pub struct CurrentTurn {
    /// Accumulated assistant text for the current turn.
    pub text: String,

    /// Accumulated reasoning / thinking text for the current turn.
    pub reasoning: String,

    /// Tool cards created by `"tool-started"` and finalised by `"tool-ended"`.
    pub tool_cards: Vec<ToolCardAccumulator>,

    /// Whether a ViewCommit already replaced the canonical view for this turn.
    pub committed: bool,

    /// Whether the turn is actively streaming (any text / tool event arrived).
    pub active: bool,

    /// Streaming sub-agent occurrences routed by agent_id / instance_id.
    ///
    /// A resumed child reuses its agent_id, so multiple stopped/running
    /// occurrences with the same ID may coexist in one parent turn.
    pub subagents: Vec<SubAgentAccumulator>,

    /// Chronological order of text flushes, tool starts, and sub-agent starts
    /// within this turn. Drive `sync_cache` to produce interleaved output.
    segments: Vec<TurnSegment>,

    /// Byte offset in `self.text` that the last `AssistantText` segment covered.
    /// Used by `flush_text_segment` to detect when new text needs a new segment.
    last_text_flush: usize,

    /// Byte offset in `self.reasoning` that the last `AssistantText` segment covered.
    /// Parallel to `last_text_flush` — each content flush records both text and
    /// reasoning boundaries so `sync_cache` can assign the correct reasoning
    /// slice to each assistant bubble.
    last_reasoning_flush: usize,

    /// ACP `messageId` of the most recent `TextChunk`. Used to detect when
    /// a new assistant message starts (message_id change → flush pending text).
    last_message_id: Option<String>,

    /// 当前未冻结（trailing）文本区域的滚动哈希——`append_text` 时增量维护。
    /// 与 `TuiAssistantBubble::compute_hash` 的文本部分共用同一公式。
    open_text_hash: u64,

    /// 当前未冻结（trailing）推理区域的滚动哈希——`append_reasoning` 时增量维护。
    open_reasoning_hash: u64,

    /// 本 turn 首次 `append_reasoning` 的时刻——推理块 `Thought for Ns` 时长的
    /// 起点（running 块 elapsed、completed 块在 flush/折叠 pass 时冻结差值）。
    /// 每次 `reset()` 清空。
    reasoning_started_at: Option<Instant>,

    /// 本 turn 首次 `append_text` 的时刻——assistant 正文 `12.4s` 时长的起点
    /// （§6.2；G-Tokens 仅 duration）。trailing bubble 构造时写入 `started_at`，
    /// 折叠 pass 在 phase 离开 PromptRunning 时冻结差值。每次 `reset()` 清空。
    text_started_at: Option<Instant>,

    /// [§6.7] 子 turn 专用冻结标记：`stop_subagent` 调用
    /// [`CurrentTurn::freeze_trailing`] 后置 `Some((正文时长 ms, 推理时长 ms))`，
    /// trailing 流式段以 Completed 形态构建（镜像顶层折叠 pass 的翻转点——
    /// 子 turn 不经过快照 pass，冻结必须在此完成）。内容不再增长，构造后
    /// 保持稳定。顶层 turn 恒 None。
    trailing_frozen: Option<(Option<u64>, Option<u64>)>,

    /// 推理结束冻结标记（方案 1：文本到达 = 本消息 thinking 块结束——模型流中
    /// thinking 必先于 text）。`Some(推理时长 ms)`：trailing 段推理块以
    /// Completed 形态渲染（`◐ Thinking…` 停止，显示 `Thought for Ns`），正文
    /// 继续流式；`None`：推理仍在进行。段切走（flush）时重置——新消息的推理
    /// 重新计时。幂等：已冻结后不再更新。
    trailing_reasoning_frozen_ms: Option<u64>,

    /// 增量 VM 缓存：索引 i 对应 `segments[i]` 的 VM，末尾元素为 trailing bubble。
    ///
    /// 使用 `im::Vector` 的原因：
    /// - 子 turn（SubAgentAccumulator）的缓存可 O(1) 克隆共享给 `TuiSubAgentGroup`，
    ///   避免每 token 深拷贝全部 child VM；
    /// - `push_view_models` 可用 `append`（O(log n) 共享元素）把缓存并入快照，
    ///   避免逐条深拷贝。
    ///
    /// 缓存内容由 `sync_cache` 增量维护——冻结段只构建一次，只有变化的部分被替换。
    cached_view_models: im::Vector<TuiRenderUnit>,

    /// 缓存与 segments/内容失同步标记：`invalidate_cache` 置位，`view_models()`
    /// 时调用 `sync_cache` 重同步。流式变更只置位，由 publication、freeze、
    /// terminal 或明确读取形成 projection barrier。
    cache_dirty: bool,
}

impl Default for CurrentTurn {
    fn default() -> Self {
        Self {
            text: String::new(),
            reasoning: String::new(),
            tool_cards: Vec::new(),
            committed: false,
            active: false,
            subagents: Vec::new(),
            segments: Vec::new(),
            last_text_flush: 0,
            last_reasoning_flush: 0,
            last_message_id: None,
            open_text_hash: 0,
            open_reasoning_hash: 0,
            reasoning_started_at: None,
            text_started_at: None,
            trailing_frozen: None,
            trailing_reasoning_frozen_ms: None,
            cached_view_models: im::Vector::new(),
            cache_dirty: false,
        }
    }
}

/// A single entry in the chronological ordering of a turn's streaming events.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TurnSegment {
    /// Text and reasoning belonging to one assistant bubble.
    /// `text_end_byte`: end (exclusive) of the text slice in `CurrentTurn.text`.
    /// `reasoning_end_byte`: end (exclusive) of the reasoning slice in `CurrentTurn.reasoning`.
    /// `text_hash` / `reasoning_hash`: 该段文本/推理区域的滚动哈希——flush 时冻结，
    /// 供缓存重建时 O(1) 取用，避免对已冻结段重新哈希。
    AssistantText {
        text_end_byte: usize,
        reasoning_end_byte: usize,
        text_hash: u64,
        reasoning_hash: u64,
        /// 该段所属 ACP messageId（flush 时冻结）——折叠覆盖键
        /// `FoldKey::Reasoning(message_id)` 用；身份字段，不进 hash。
        message_id: Option<String>,
        /// 本段推理区的冻结时长（毫秒）——flush 时刻距 `reasoning_started_at`
        /// 的差值；completed 推理块 `Thought for Ns` 显示用（§6.3）。
        /// 无推理的段为 `None`。
        reasoning_duration_ms: Option<u64>,
    },
    /// Tool card reference to `CurrentTurn.tool_cards[tool_idx]`.
    Tool { tool_idx: usize },
    /// Sub-agent reference to `CurrentTurn.subagents[subagent_idx]`.
    SubAgent { subagent_idx: usize },
    /// System note（如 cache 命中率警告、budget 警告）——直接嵌入当前 turn 时序位置。
    /// 与 Tool/SubAgent 不同，SystemNote 数据完全自包含，无需外部 Vec 索引。
    SystemNote {
        text: String,
        level: TuiNoteLevel,
        content_hash: u64,
    },
}

impl CurrentTurn {
    /// Create a new empty `CurrentTurn`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the cached ViewModels dirty（下次 `view_models()` 时增量重同步）。
    ///
    /// 语义与旧版一致：调用后 `view_models()` 必然反映最新状态；实现上不再
    /// 清空缓存，而是由 `sync_cache` 只修补变化的部分。acp_bridge 的 1s tick
    /// 依赖此入口刷新运行中工具卡片的时长。
    pub(crate) fn invalidate_cache(&mut self) {
        self.cache_dirty = true;
    }

    pub(crate) fn has_unprojected_changes(&self) -> bool {
        self.cache_dirty
    }

    /// Mark the turn as no longer active (e.g. on `"turn-interrupted"`).
    pub fn deactivate(&mut self) {
        self.active = false;
        self.invalidate_cache();
    }

    /// Mark current turn as committed by a canonical ViewCommit snapshot.
    pub fn mark_committed(&mut self) {
        self.text.clear();
        self.reasoning.clear();
        self.tool_cards.clear();
        self.subagents.clear();
        self.segments.clear();
        self.last_text_flush = 0;
        self.last_reasoning_flush = 0;
        self.last_message_id = None;
        self.open_text_hash = 0;
        self.open_reasoning_hash = 0;
        self.reasoning_started_at = None;
        self.text_started_at = None;
        self.trailing_frozen = None;
        self.cached_view_models = im::Vector::new();
        self.cache_dirty = false;
        self.active = false;
        self.committed = true;
    }

    /// [§6.7] 冻结 trailing 流式段（镜像顶层折叠 pass 的翻转点语义）。
    ///
    /// 顶层 turn 的冻结由 `apply_fold_pass` 在 phase 离开 PromptRunning 时对
    /// 快照 VM 完成；子 turn（SubAgentAccumulator）不经过快照 pass，`stop_subagent`
    /// 必须在此把 `text_started_at`/`reasoning_started_at` 一次性换算为冻结
    /// 时长并清除——此后 trailing bubble 以 Completed/Collapsed 形态构建，
    /// elapsed 不再增长（详情面板不再出现永久的 `◐ Thinking… Ns`）。
    /// 无 trailing 内容时为 no-op（幂等：重复 stop 安全）。
    pub(crate) fn freeze_trailing(&mut self) {
        if self.text.len() > self.last_text_flush
            || self.reasoning.len() > self.last_reasoning_flush
        {
            self.trailing_frozen = Some((
                self.text_started_at.map(|t| t.elapsed().as_millis() as u64),
                self.reasoning_started_at
                    .map(|t| t.elapsed().as_millis() as u64),
            ));
        }
        self.text_started_at = None;
        self.reasoning_started_at = None;
        self.cache_dirty = true;
    }

    /// Clear current turn without marking a canonical commit boundary.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Whether this turn has no pending incremental ViewModels.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.reasoning.is_empty()
            && self.tool_cards.is_empty()
            && self.subagents.is_empty()
            && self.cached_view_models.is_empty()
    }

    pub fn has_running_bash_tool(&self) -> bool {
        self.tool_cards
            .iter()
            .any(|t| t.tool_name == "Bash" && t.output_summary.is_none())
            || self
                .subagents
                .iter()
                .any(|s| s.child_turn.has_running_bash_tool())
    }
}
