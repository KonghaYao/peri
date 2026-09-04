use super::tool_card::{SubAgentAccumulator, ToolCardAccumulator, build_tool_card};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiNoteLevel, TuiReasoningBlock, TuiRenderUnit, entry_status_code,
    fold_for_status, fold_state_code, tui_hash_combine, tui_hash_roll_update,
};
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

    /// If text has grown since the last `AssistantText` segment, push a new
    /// segment capturing the delta (both text and reasoning boundaries).
    ///
    /// 冻结时把当前 open 区域的滚动哈希存入段记录——此后该段内容不再变化，
    /// 缓存重建可直接 O(1) 取用，无需对冻结段重新哈希。
    /// 推理时长同刻冻结（§6.3 `Thought for Ns`）——flush 后不再增长。
    fn flush_text_segment(&mut self) {
        let current_text = self.text.len();
        let current_reasoning = self.reasoning.len();
        if current_text > self.last_text_flush || current_reasoning > self.last_reasoning_flush {
            let reasoning_duration_ms = self
                .reasoning_started_at
                .map(|t| t.elapsed().as_millis() as u64);
            // [Fix think-end] flush 把旧 trailing 变成新段：缓存尾部残留的是
            // flush 前的 trailing bubble（推理块 Running 形态），索引错位后
            // sync_cache 的 `len() <= i` 守卫会复用陈旧缓存——推理段恒 Running，
            // 动画空转到 turn 结束折叠 pass 才冻结（思考→工具场景实测必现）。
            // 段计数以 push 前的 segments.len() 为基准：缓存 = 段数（无 trailing）
            // 或段数+1（有 trailing），flush 后缓存应回落到新段数。丢弃尾部
            // 失效元素（flush 前 trailing 至多一个），保留历史冻结段。
            let seg_len_before_push = self.segments.len();
            self.segments.push(TurnSegment::AssistantText {
                text_end_byte: current_text,
                reasoning_end_byte: current_reasoning,
                text_hash: self.open_text_hash,
                reasoning_hash: self.open_reasoning_hash,
                // flush 发生在 last_message_id 更新之前（append_text/append_reasoning
                // 先 flush 旧段再换新 id）——此处记录的是本段自己的 message id。
                message_id: self.last_message_id.clone(),
                reasoning_duration_ms,
            });
            while self.cached_view_models.len() > seg_len_before_push {
                self.cached_view_models.pop_back();
            }
            self.last_text_flush = current_text;
            self.last_reasoning_flush = current_reasoning;
            self.open_text_hash = 0;
            self.open_reasoning_hash = 0;
            // 段切走后新消息的推理重新计时（幂等：无增长时 no-op 不重置）。
            self.trailing_reasoning_frozen_ms = None;
        }
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

    /// Append a text chunk from `"text-chunk"`.
    ///
    /// If `message_id` differs from the previous chunk, a new assistant message
    /// has started — the pending text is flushed as a separate segment so the
    /// renderer can show it in its own bubble rather than merging it into one blob.
    ///
    /// 推理结束推断（方案 1）：模型流中 thinking block 必先于 text block——
    /// 文本到达即意味着本消息的推理已结束。冻结 trailing 推理块（`◐ Thinking…`
    /// 动画停止，显示 `Thought for Ns`），正文继续流式。与 messageId 变化
    /// flush 互补：messageId 缺失时（v1 兼容路径）同样生效；幂等（已冻结后
    /// no-op，连续文本块不重复冻结）。
    pub fn append_text(&mut self, t: &str, message_id: Option<&str>) {
        if let Some(prev_id) = &self.last_message_id
            && let Some(new_id) = message_id
            && prev_id != new_id
        {
            self.flush_text_segment();
        }
        if self.trailing_reasoning_frozen_ms.is_none()
            && self.reasoning.len() > self.last_reasoning_flush
        {
            // 冻结推理时长（reasoning_started_at 仍存活：freeze_trailing/折叠 pass
            // 的换算不依赖本字段被清除，两套机制互不干扰）。
            self.trailing_reasoning_frozen_ms = self
                .reasoning_started_at
                .map(|t| t.elapsed().as_millis() as u64);
        }
        self.last_message_id = message_id.map(|s| s.to_string());
        self.text.push_str(t);
        self.open_text_hash = tui_hash_roll_update(self.open_text_hash, t);
        self.text_started_at.get_or_insert_with(Instant::now);
        self.active = true;
        self.invalidate_cache();
    }

    /// Append a reasoning chunk from `"reasoning-chunk"`.
    ///
    /// Same `message_id` semantics as `append_text`: a new ID triggers
    /// a text segment flush so reasoning and text for different messages
    /// are separated.
    pub fn append_reasoning(&mut self, t: &str, message_id: Option<&str>) {
        if let Some(prev_id) = &self.last_message_id
            && let Some(new_id) = message_id
            && prev_id != new_id
        {
            self.flush_text_segment();
        }
        self.last_message_id = message_id.map(|s| s.to_string());
        self.reasoning.push_str(t);
        self.open_reasoning_hash = tui_hash_roll_update(self.open_reasoning_hash, t);
        self.reasoning_started_at.get_or_insert_with(Instant::now);
        self.active = true;
        self.invalidate_cache();
    }

    /// Begin a new tool card from `"tool-started"`.
    ///
    /// Flushes any pending text as a segment BEFORE pushing the tool,
    /// so text spoken before the tool call appears in its own bubble.
    pub fn start_tool(&mut self, tool: ToolCardAccumulator) {
        // 防御：相同 tool_id 不应重复 start（同一轮内 tool_id 唯一）。
        // [Fix think-end] agent 侧提前 ToolStarted（工具块开始即发，参数尚未
        // 流式生成 → raw_input=Null）与 dispatch 的正式 ToolStarted（参数完整）
        // 同 id 先后到达：只升级 input（raw_input/input_summary/presentation），
        // 不重建卡片——保留 started_at/时长语义，TUI 侧冻结点由"工具卡片
        // 出现"提前到"thinking 真实结束"。
        if let Some(existing) = self
            .tool_cards
            .iter_mut()
            .find(|t| t.tool_id == tool.tool_id)
        {
            if !tool.raw_input.is_null() && existing.raw_input.is_null() {
                tracing::debug!(
                    tool_id = %tool.tool_id,
                    tool_name = %tool.tool_name,
                    "CurrentTurn::start_tool: 提前 ToolStarted 升级 input"
                );
                existing.raw_input = tool.raw_input;
                existing.input_summary = tool.input_summary;
                existing.presentation = tool.presentation;
                self.invalidate_cache();
            }
            return;
        }
        self.flush_text_segment();
        let idx = self.tool_cards.len();
        self.segments.push(TurnSegment::Tool { tool_idx: idx });
        self.tool_cards.push(tool);
        self.active = true;
        self.invalidate_cache();
    }

    /// Finalise an existing running tool card from `"tool-ended"`.
    ///
    /// Returns `true` only when this call transitions the matching card from running
    /// to finished. Unknown and duplicate end events are no-ops.
    pub fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) -> bool {
        let Some(t) = self
            .tool_cards
            .iter_mut()
            .find(|t| t.tool_id == tool_id && t.output_summary.is_none())
        else {
            return false;
        };
        t.output_summary = Some(output);
        t.is_error = is_error;
        // [G-started_at] 完成时刻冻结时长——running→completed 不重建 accumulator，
        // completed 显示用同源 started_at 的冻结差值（不再增长）。
        t.completed_duration_ms = Some(t.started_at.elapsed().as_millis() as u64);
        self.invalidate_cache();
        true
    }

    /// Begin a new sub-agent group from `"subagent-started"`.
    ///
    /// Flushes any pending text before the sub-agent boundary.
    pub fn start_subagent(&mut self, agent_id: String, agent_name: String) {
        // Duplicate Start for the same live occurrence is idempotent. A resume,
        // however, reuses child_thread_id after the previous occurrence stopped;
        // it must create a fresh group and claim the new Agent ToolCard.
        if self
            .subagents
            .iter()
            .any(|s| s.agent_id == agent_id && s.is_running)
        {
            return;
        }
        self.flush_text_segment();
        let idx = self.subagents.len();

        // 前向扫描找第一个未 claim 的 Agent ToolCard，在其后插入 SubAgent 段。
        // 防止多 Agent 同 turn 时 SubAgent 段全部 append 到末尾导致
        // "agent agent tools tools" 而非 "agent tools agent tools"。
        let mut insert_at: Option<(usize, usize)> = None; // (seg_pos, tool_idx)
        for (i, seg) in self.segments.iter().enumerate() {
            if let TurnSegment::Tool { tool_idx } = seg
                && let Some(tc) = self.tool_cards.get(*tool_idx)
                && tc.tool_name == "Agent"
                && !tc.claimed_by_subagent
            {
                insert_at = Some((i + 1, *tool_idx));
                break;
            }
        }

        if let Some((seg_pos, tool_idx)) = insert_at {
            self.tool_cards[tool_idx].claimed_by_subagent = true;
            self.segments
                .insert(seg_pos, TurnSegment::SubAgent { subagent_idx: idx });
            // 段列表中部插入会破坏 segment↔cache 的索引对齐——清空缓存整体重建。
            // 该操作低频（每 subagent 一次），O(total) 成本可接受。
            self.cached_view_models = im::Vector::new();
        } else {
            self.segments
                .push(TurnSegment::SubAgent { subagent_idx: idx });
        }

        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// 在 current_turn 内部时序位置注入一条 SystemNote（如 final cache coverage 警告）。
    ///
    /// 先 flush 挂起的 text segment，再将 SystemNote 作为独立 segment 追加。
    /// 这样 SystemNote 天然位于已产出 AI 内容之后、后续内容之前，
    /// 不再依赖 `flush_current_turn()` 及其 `has_running_subagent` 守卫。
    pub fn push_system_note(&mut self, text: String, level: TuiNoteLevel, content_hash: u64) {
        self.flush_text_segment();
        self.segments.push(TurnSegment::SystemNote {
            text,
            level,
            content_hash,
        });
        self.active = true;
        self.invalidate_cache();
    }

    /// [诊断] 返回当前所有 SubAgentAccumulator 的 agent_id 列表。
    pub fn subagent_ids(&self) -> Vec<&str> {
        self.subagents.iter().map(|s| s.agent_id.as_str()).collect()
    }

    /// Mark a sub-agent group as done from `"subagent-stopped"`.
    ///
    /// `is_error` 是 parent 终态的唯一事实源（agent 层语义：Completed→false、
    /// Interrupted/Error→true）；`result` 仅在 genuine error 且非空白（trim
    /// 后非空）时保存为可见原因（`error_reason`），completed parent 即使有
    /// 失败 child tool 也不携带 parent error。保存的是原始未 trim 的 result
    /// （空白仅用于判缺，不修改展示文本）。
    pub fn stop_subagent(&mut self, agent_id: &str, is_error: bool, result: &str) {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.is_running = false;
            s.is_error = is_error;
            s.error_reason = (is_error && !result.trim().is_empty()).then(|| result.to_string());
            // [§6.7] 冻结子 turn 的 trailing 流式段——子 turn 不经过快照折叠
            // pass，不冻结则 trailing bubble 保持 Running 形态（started_at
            // 存活、elapsed 持续增长），详情面板对已完成 subagent 渲染永久的
            // `◐ Thinking… Ns`。
            s.child_turn.freeze_trailing();
            // 子 turn 必须同时 deactivate：ToolStarted 无 ToolEnded 直接停止时，
            // active 残留 true 会让 build_tool_card 以 `turn_active` 把无
            // output_summary 的工具卡保持 Running（is_running = active && 无输出）。
            s.child_turn.deactivate();
            s.cached_view_model.replace(None);
            self.invalidate_cache();
        }
    }

    /// Route text chunks into a sub-agent child message.
    pub fn append_subagent_text(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.append_text(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route reasoning chunks into a sub-agent child message.
    pub fn append_subagent_reasoning(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.append_reasoning(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route tool start into a sub-agent child message.
    pub fn start_subagent_tool(&mut self, agent_id: &str, tool: ToolCardAccumulator) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            s.start_tool(tool);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            // [诊断] 路由失败时记录所有已注册的 agent_id
            let registered: Vec<&str> =
                self.subagents.iter().map(|s| s.agent_id.as_str()).collect();
            tracing::debug!(
                agent_id = %agent_id,
                registered = ?registered,
                "start_subagent_tool: agent_id not found in registered SubAgentAccumulators"
            );
            false
        }
    }

    /// Route tool end into a sub-agent child message.
    pub fn end_subagent_tool(
        &mut self,
        agent_id: &str,
        tool_id: &str,
        output: String,
        is_error: bool,
    ) -> bool {
        if let Some(s) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == agent_id)
        {
            let ended = s.end_tool(tool_id, output, is_error);
            if ended {
                self.active = true;
                self.invalidate_cache();
            }
            ended
        } else {
            false
        }
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

    /// Accessor: returns cached ViewModels.
    ///
    /// 缓存由 `sync_cache` 增量维护：流式变更在 mutation 时 eager sync，
    /// `invalidate_cache`（如 acp_bridge 1s tick 刷新工具时长）置位后在下次
    /// 调用时重同步。返回的 `im::Vector` 可 O(1) 克隆共享。
    pub fn view_models(&mut self) -> &im::Vector<TuiRenderUnit> {
        if self.cache_dirty {
            self.sync_cache();
        }
        &self.cached_view_models
    }

    /// 构造 reasoning 块 + 内容哈希。
    ///
    /// `text_hash`/`reasoning_hash` 是文本/推理区域的滚动哈希（增量维护的 open
    /// 值或冻结段存储值），组合公式与 [`TuiAssistantBubble::compute_hash`] 完全
    /// 一致——保证增量路径与从零重建（recompute_hash）产出相同的 hash。
    ///
    /// `reasoning_running`：true = trailing 流式段（status=Running、fold=Preview、
    /// started_at=推理起点）；false = 冻结段（status=Completed、fold=Collapsed、
    /// duration_ms=flush 时刻冻结值）。折叠 pass 在 phase 离开 PromptRunning 时
    /// 把 trailing 段翻转成 Completed。bubble 的 `message_id` 由调用方直接赋值
    /// （身份字段，不进 hash）。
    ///
    /// 方案 1 混合形态：推理已结束而正文仍流式时，`reasoning_running=false`
    /// 且 `text_started_at=Some`——推理块 Completed + 正文 Running（`◐ Thinking…`
    /// 停止，正文继续增长）。冻结段两个标志恒为冻结态。
    ///
    /// `text_started_at`：trailing 段的本 bubble 正文开始时刻——running 时写入
    /// `TuiAssistantBubble.started_at`；冻结段传 None（正文时长由折叠 pass 在
    /// 翻转点冻结，镜像 reasoning 机制）。
    ///
    /// [§6.3] 空 reasoning：reasoning_running 且 reasoning 为空时仍产出空文本
    /// 的推理块（`◐ Thinking…` 占位行，不出现空白 block）；`!reasoning_running`
    /// 的空 reasoning 返回 `None`（冻结段无占位，避免历史噪音）。
    fn build_bubble_parts(
        reasoning: &str,
        text_hash: u64,
        reasoning_hash: u64,
        reasoning_running: bool,
        reasoning_started_at: Option<Instant>,
        reasoning_duration_ms: Option<u64>,
        text_started_at: Option<Instant>,
    ) -> (Option<TuiReasoningBlock>, u64) {
        let block = if reasoning.is_empty() && !reasoning_running {
            None
        } else {
            let status = if reasoning_running {
                EntryStatus::Running
            } else {
                EntryStatus::Completed
            };
            Some(TuiReasoningBlock {
                text: reasoning.to_string(),
                fold: fold_for_status(FoldTarget::Reasoning, status),
                status,
                is_running: reasoning_running,
                started_at: if reasoning_running {
                    reasoning_started_at
                } else {
                    None
                },
                duration_ms: if reasoning_running {
                    None
                } else {
                    reasoning_duration_ms
                },
            })
        };
        // [G1] 与 TuiAssistantBubble::compute_hash 同序同码——fold/status/is_running/
        // duration 逐项组合，末尾追加正文时长秒数（running 取 started_at 已耗时，
        // 冻结段取 duration_ms/1000，均秒取整；None→0）与冻结判别位
        // （`text_started_at.is_none()`，镜像 recompute_hash 的 started_at 口径），
        // 保证增量路径与 recompute_hash 产出相同 hash。
        let text_duration_secs = text_started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let text_frozen = u64::from(text_started_at.is_none());
        let content_hash = match block.as_ref() {
            Some(r) => {
                let mut h = tui_hash_combine(
                    tui_hash_combine(text_hash, reasoning_hash),
                    fold_state_code(r.fold),
                );
                h = tui_hash_combine(h, entry_status_code(r.status));
                h = tui_hash_combine(h, u64::from(r.is_running));
                h = tui_hash_combine(h, r.duration_code());
                h = tui_hash_combine(h, text_duration_secs);
                tui_hash_combine(h, text_frozen)
            }
            None => {
                let h = tui_hash_combine(text_hash, text_duration_secs);
                tui_hash_combine(h, text_frozen)
            }
        };
        (block, content_hash)
    }

    /// 将 `cached_view_models` 与 `segments`/内容增量对齐——只重建/替换变化的
    /// 部分（trailing bubble、运行中或刚结束的工具卡、内容变化的 subagent 组），
    /// 冻结的 AssistantText 段与未变化的元素直接复用缓存。
    ///
    /// 每 token 成本 O(变化量 + 段数扫描)，取代旧版每 token 全量重建（O(总内容)
    /// 文本拷贝 + 全量 format!/hash 的 O(N²) 累积）。
    fn sync_cache(&mut self) {
        #[cfg(test)]
        {
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::Projection,
                1,
            );
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::ProjectionCopiedBytes,
                (self.text.len() + self.reasoning.len()) as u64,
            );
        }
        use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiSystemNote};

        let mut prev_text_end: usize = 0;
        let mut prev_reasoning_end: usize = 0;

        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                TurnSegment::AssistantText {
                    text_end_byte,
                    reasoning_end_byte,
                    text_hash,
                    reasoning_hash,
                    message_id,
                    reasoning_duration_ms,
                } => {
                    let text_end = (*text_end_byte).min(self.text.len());
                    let reason_end = (*reasoning_end_byte).min(self.reasoning.len());
                    // 冻结段只构建一次，此后直接复用缓存（内容不再变化）。
                    if self.cached_view_models.len() <= i {
                        let text_slice = &self.text[prev_text_end..text_end];
                        let reasoning_slice = &self.reasoning[prev_reasoning_end..reason_end];
                        let (reasoning, content_hash) = Self::build_bubble_parts(
                            reasoning_slice,
                            *text_hash,
                            *reasoning_hash,
                            false,
                            None,
                            *reasoning_duration_ms,
                            None,
                        );
                        self.cached_view_models
                            .push_back(TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                                text: text_slice.to_string(),
                                reasoning,
                                message_id: message_id.clone(),
                                // 冻结段无正文时长起点——时长由折叠 pass 在翻转点
                                // 对 trailing bubble 冻结；此处恒 None（G-Tokens）。
                                started_at: None,
                                duration_ms: None,
                                content_hash,
                            }));
                    }
                    prev_text_end = text_end;
                    prev_reasoning_end = reason_end;
                }
                TurnSegment::Tool { tool_idx } => {
                    if let Some(t) = self.tool_cards.get(*tool_idx) {
                        // 运行中卡片每 sync 重建（刷新 duration，hash 按秒变化）；
                        // 已结束卡片仅在 output 变化时重建一次。
                        let needs_rebuild = match self.cached_view_models.get(i) {
                            Some(TuiRenderUnit::TuiToolCard(c)) => {
                                c.is_running
                                    || Some(c.output_summary.as_str())
                                        != t.output_summary.as_deref()
                            }
                            _ => true,
                        };
                        if needs_rebuild {
                            let card = build_tool_card(t, self.active);
                            if self.cached_view_models.len() <= i {
                                self.cached_view_models
                                    .push_back(TuiRenderUnit::TuiToolCard(card));
                            } else {
                                self.cached_view_models
                                    .set(i, TuiRenderUnit::TuiToolCard(card));
                            }
                        }
                    }
                }
                TurnSegment::SubAgent { subagent_idx } => {
                    if let Some(s) = self.subagents.get_mut(*subagent_idx) {
                        let group_vm = s.view_model();
                        // O(1) hash 比较——未变化的 subagent 直接跳过 set()，
                        // 已变化的替换为新组（im::Vector set 走 COW，共享未变子节点）。
                        let changed = self
                            .cached_view_models
                            .get(i)
                            .is_none_or(|old| old.content_hash() != group_vm.content_hash());
                        if changed {
                            if self.cached_view_models.len() <= i {
                                self.cached_view_models.push_back(group_vm);
                            } else {
                                self.cached_view_models.set(i, group_vm);
                            }
                        }
                    }
                }
                TurnSegment::SystemNote {
                    text,
                    level,
                    content_hash,
                } => {
                    if self.cached_view_models.len() <= i {
                        self.cached_view_models
                            .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                                text: text.clone(),
                                level: level.clone(),
                                content_hash: *content_hash,
                            }));
                    }
                }
            }
        }

        // Trailing bubble（最后一个段之后仍未冻结的内容）——文本/推理增长时重建。
        // 长度比对是 O(1) 的变化检测：该区域 append-only，长度变 ⟺ 内容变。
        let has_trailing = self.text.len() > self.last_text_flush
            || self.reasoning.len() > self.last_reasoning_flush;
        if has_trailing {
            let trailing_idx = self.segments.len();
            let trailing_len_changed = match self.cached_view_models.get(trailing_idx) {
                Some(TuiRenderUnit::TuiAssistantBubble(b)) => {
                    b.text.len() != self.text.len() - self.last_text_flush
                        || b.reasoning.as_ref().map(|r| r.text.len()).unwrap_or(0)
                            != self.reasoning.len() - self.last_reasoning_flush
                }
                _ => true,
            };
            // [Fix §6.7] 冻结待消费（`freeze_trailing` 置位）：冻结只改 VM 形态
            // （started_at→None / duration_ms→Some / running→completed），不改
            // 文本长度——长度门控恒 false 会保留陈旧的 Running 形态 bubble
            // （详情面板对已完成 subagent 永久 `◐ Thinking… Ns`）。take() 消费
            // 后恢复长度门控（冻结重建恰一次，幂等）。
            let pending_freeze = self.trailing_frozen.is_some();
            if trailing_len_changed || pending_freeze {
                let text_slice = &self.text[self.last_text_flush..];
                let reasoning_slice = &self.reasoning[self.last_reasoning_flush..];
                // [§6.7] 冻结形态（stop_subagent 后）：Completed / Collapsed /
                // 冻结时长——`freeze_trailing` 已把 started_at 换算为 ms 并清除，
                // 此处直接构建（不经过顶层折叠 pass）。
                let trailing = if let Some((text_dur, reasoning_dur)) = self.trailing_frozen.take()
                {
                    let (reasoning, content_hash) = Self::build_bubble_parts(
                        reasoning_slice,
                        self.open_text_hash,
                        self.open_reasoning_hash,
                        false,
                        None,
                        reasoning_dur,
                        None,
                    );
                    let mut bubble = TuiAssistantBubble {
                        text: text_slice.to_string(),
                        reasoning,
                        message_id: self.last_message_id.clone(),
                        started_at: None,
                        duration_ms: text_dur,
                        content_hash,
                    };
                    // [G1] 单点重算：与折叠 pass 冻结后 recompute_hash 公式一致
                    // （build_bubble_parts 的 !running 路径 text_duration=0，
                    // 不含冻结正文时长）。
                    bubble.recompute_hash();
                    TuiRenderUnit::TuiAssistantBubble(bubble)
                } else {
                    let (reasoning, content_hash) = Self::build_bubble_parts(
                        reasoning_slice,
                        self.open_text_hash,
                        self.open_reasoning_hash,
                        // 文本到达后推理已结束：推理块 Completed、正文继续 Running
                        self.trailing_reasoning_frozen_ms.is_none(),
                        self.reasoning_started_at,
                        self.trailing_reasoning_frozen_ms,
                        self.text_started_at,
                    );
                    TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                        text: text_slice.to_string(),
                        reasoning,
                        message_id: self.last_message_id.clone(),
                        started_at: self.text_started_at,
                        duration_ms: None,
                        content_hash,
                    })
                };
                if self.cached_view_models.len() <= trailing_idx {
                    self.cached_view_models.push_back(trailing);
                } else {
                    self.cached_view_models.set(trailing_idx, trailing);
                }
            }
        }

        // 后处理：将 Agent 工具卡片的 tool_calls_count 与紧随的 SubAgent 组配对。
        // 匹配逻辑：TuiToolCard(tool_name="Agent") 紧接着 TuiSubAgentGroup。
        self.pair_agent_tool_cards();

        self.cache_dirty = false;
    }

    /// 折叠归一化已删除（Slice 2）：折叠策略收敛到 `push_view_models` 的
    /// `apply_fold_pass` 单点（spec §7 表 + FOLD_OVERRIDES），缓存层不再内联
    /// 折叠决策——`sync_cache` 只负责按内容构建 VM，折叠由快照 pass 统一驱动。
    ///
    /// Agent 工具卡片与紧随的 SubAgent 组配对（tool_calls_count）。
    fn pair_agent_tool_cards(&mut self) {
        let mut updates: Vec<(usize, usize)> = Vec::new(); // (index, tool_count)
        let n = self.cached_view_models.len();
        for i in 0..n.saturating_sub(1) {
            if let (
                TuiRenderUnit::TuiToolCard(agent_card),
                TuiRenderUnit::TuiSubAgentGroup(subagent_group),
            ) = (&self.cached_view_models[i], &self.cached_view_models[i + 1])
                && agent_card.tool_name == "Agent"
                && agent_card.is_running
            {
                let tool_count = subagent_group
                    .view_models
                    .iter()
                    .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
                    .count();
                if tool_count > 0 && agent_card.tool_calls_count != tool_count {
                    updates.push((i, tool_count));
                }
            }
        }
        for (i, tool_count) in updates {
            if let TuiRenderUnit::TuiToolCard(card) = &self.cached_view_models[i] {
                let mut updated = card.clone();
                updated.tool_calls_count = tool_count;
                self.cached_view_models
                    .set(i, TuiRenderUnit::TuiToolCard(updated));
            }
        }
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
