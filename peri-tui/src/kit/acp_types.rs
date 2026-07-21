//! ACP 流式数据类型——`CurrentTurn` + `ToolCardAccumulator` + `AcpEventData`。
//!
//! S11 起类型定义集中在本模块，不再通过 re-export 分散到其他模块。
//!
//! ## 设计
//!
//! - **纯数据 + 方法**：所有字段为 String/Vec/bool/u32/serde_json::Value，
//!   天然 Send+Sync+'static
//! - **依赖**：仅 `crate::kit::tui_render_unit::TuiRenderUnit` 和
//!   `peri_acp_types::event_data::*`（workspace crate，非 legacy）
//! - **零运行时依赖**：无 terminal / network / IO，可独立测试

use crate::kit::stream_data::*;
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit};
use peri_acp_types::event_data::*;
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
/// `segments` records this chronological order so that `build_view_models` can
/// create separate `TuiAssistantBubble` entries for text before and after each
/// tool/sub-agent boundary, instead of merging everything into one fat bubble.
#[derive(Debug, Clone, Default)]
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

    /// Streaming sub-agent cards keyed by agent_id / instance_id.
    pub subagents: Vec<SubAgentAccumulator>,

    /// Chronological order of text flushes, tool starts, and sub-agent starts
    /// within this turn. Drive `build_view_models` to produce interleaved output.
    segments: Vec<TurnSegment>,

    /// Byte offset in `self.text` that the last `AssistantText` segment covered.
    /// Used by `flush_text_segment` to detect when new text needs a new segment.
    last_text_flush: usize,

    /// Byte offset in `self.reasoning` that the last `AssistantText` segment covered.
    /// Parallel to `last_text_flush` — each content flush records both text and
    /// reasoning boundaries so `build_view_models` can assign the correct reasoning
    /// slice to each assistant bubble.
    last_reasoning_flush: usize,

    /// ACP `messageId` of the most recent `TextChunk`. Used to detect when
    /// a new assistant message starts (message_id change → flush pending text).
    last_message_id: Option<String>,

    /// Cached ViewModels built from streaming data (populated by `build_view_models`).
    ///
    /// Cleared whenever new streaming data arrives (text/reasoning/tool events),
    /// and rebuilt on the next call to `view_models()`.
    cached_view_models: Vec<TuiRenderUnit>,
}

/// A single entry in the chronological ordering of a turn's streaming events.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TurnSegment {
    /// Text and reasoning belonging to one assistant bubble.
    /// `text_end_byte`: end (exclusive) of the text slice in `CurrentTurn.text`.
    /// `reasoning_end_byte`: end (exclusive) of the reasoning slice in `CurrentTurn.reasoning`.
    AssistantText {
        text_end_byte: usize,
        reasoning_end_byte: usize,
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
    fn flush_text_segment(&mut self) {
        let current_text = self.text.len();
        let current_reasoning = self.reasoning.len();
        if current_text > self.last_text_flush || current_reasoning > self.last_reasoning_flush {
            self.segments.push(TurnSegment::AssistantText {
                text_end_byte: current_text,
                reasoning_end_byte: current_reasoning,
            });
            self.last_text_flush = current_text;
            self.last_reasoning_flush = current_reasoning;
        }
    }

    /// Invalidate the cached ViewModels (call after any streaming data mutation).
    pub(crate) fn invalidate_cache(&mut self) {
        self.cached_view_models.clear();
    }

    /// Append a text chunk from `"text-chunk"`.
    ///
    /// If `message_id` differs from the previous chunk, a new assistant message
    /// has started — the pending text is flushed as a separate segment so the
    /// renderer can show it in its own bubble rather than merging it into one blob.
    pub fn append_text(&mut self, t: &str, message_id: Option<&str>) {
        if let Some(prev_id) = &self.last_message_id
            && let Some(new_id) = message_id
            && prev_id != new_id
        {
            self.flush_text_segment();
        }
        self.last_message_id = message_id.map(|s| s.to_string());
        self.text.push_str(t);
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
        self.active = true;
        self.invalidate_cache();
    }

    /// Begin a new tool card from `"tool-started"`.
    ///
    /// Flushes any pending text as a segment BEFORE pushing the tool,
    /// so text spoken before the tool call appears in its own bubble.
    pub fn start_tool(&mut self, tool: ToolCardAccumulator) {
        // 防御：相同 tool_id 不应重复 start（同一轮内 tool_id 唯一）
        if self.tool_cards.iter().any(|t| t.tool_id == tool.tool_id) {
            tracing::debug!(
                tool_id = %tool.tool_id,
                tool_name = %tool.tool_name,
                "CurrentTurn::start_tool: 重复 tool_id，跳过"
            );
            return;
        }
        self.flush_text_segment();
        let idx = self.tool_cards.len();
        self.segments.push(TurnSegment::Tool { tool_idx: idx });
        self.tool_cards.push(tool);
        self.active = true;
        self.invalidate_cache();
    }

    /// Finalise an existing tool card from `"tool-ended"`.
    ///
    /// No-op if `tool_id` does not match any open card.
    pub fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) {
        if let Some(t) = self.tool_cards.iter_mut().find(|t| t.tool_id == tool_id) {
            t.output_summary = Some(output);
            t.is_error = is_error;
            self.invalidate_cache();
        }
    }

    /// Begin a new sub-agent group from `"subagent-started"`.
    ///
    /// Flushes any pending text before the sub-agent boundary.
    pub fn start_subagent(&mut self, agent_id: String, agent_name: String) {
        if self.subagents.iter().any(|s| s.agent_id == agent_id) {
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
        } else {
            self.segments
                .push(TurnSegment::SubAgent { subagent_idx: idx });
        }

        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// 在 current_turn 内部时序位置注入一条 SystemNote（如 cache 命中率警告）。
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
    pub fn stop_subagent(&mut self, agent_id: &str) {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.is_running = false;
            s.cached_view_model.replace(None);
            self.invalidate_cache();
        }
    }

    /// Route text chunks into a sub-agent child message.
    pub fn append_subagent_text(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
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
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
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
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
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
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.end_tool(tool_id, output, is_error);
            self.active = true;
            self.invalidate_cache();
            true
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
        self.cached_view_models.clear();
        self.active = false;
        self.committed = true;
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

    /// Accessor: returns cached ViewModels, building them on first call.
    ///
    /// The cache is invalidated whenever streaming data changes (text/reasoning/
    /// tool events), so this always reflects the current turn state.
    pub fn view_models(&mut self) -> &[TuiRenderUnit] {
        if self.cached_view_models.is_empty()
            && (self.active
                || !self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tool_cards.is_empty()
                || !self.subagents.is_empty())
        {
            self.build_view_models();
        }
        &self.cached_view_models
    }

    /// Build incremental ViewModels from accumulated streaming data into cache.
    ///
    /// Walks through `self.segments` in chronological order, creating separate
    /// `TuiAssistantBubble` entries for text before and after each tool/sub-agent
    /// boundary. Each bubble gets its own reasoning slice from `self.reasoning`,
    /// bounded by `reasoning_end_byte` in the current segment.
    /// Any trailing content past the last segment is rendered as a final bubble.
    fn build_view_models(&mut self) {
        use crate::kit::tui_render_unit::{
            TuiAssistantBubble, TuiReasoningBlock, TuiSystemNote, TuiToolCard, tui_hash_str,
        };

        let mut vms: Vec<TuiRenderUnit> = Vec::new();
        let mut prev_text_end: usize = 0;
        let mut prev_reasoning_end: usize = 0;

        // build_reasoning 构造初始 reasoning block（collapsed=false），hash 由
        // TuiAssistantBubble::compute_hash 计算——保证公式与 recompute_hash 一致，
        // 后续 push_view_models 修改 collapsed 时重算结果可控。
        let build_reasoning = |text: &str, reasoning: &str| -> (Option<TuiReasoningBlock>, u64) {
            let block = if reasoning.is_empty() {
                None
            } else {
                Some(TuiReasoningBlock {
                    text: reasoning.to_string(),
                    collapsed: false,
                })
            };
            let content_hash = TuiAssistantBubble::compute_hash(text, block.as_ref());
            (block, content_hash)
        };

        for segment in &self.segments {
            match segment {
                TurnSegment::AssistantText {
                    text_end_byte,
                    reasoning_end_byte,
                } => {
                    let text_end = (*text_end_byte).min(self.text.len());
                    let reason_end = (*reasoning_end_byte).min(self.reasoning.len());
                    if text_end > prev_text_end || reason_end > prev_reasoning_end {
                        let text_slice = &self.text[prev_text_end..text_end];
                        let reasoning_slice = &self.reasoning[prev_reasoning_end..reason_end];
                        let (reasoning, content_hash) =
                            build_reasoning(text_slice, reasoning_slice);
                        vms.push(TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                            text: text_slice.to_string(),
                            reasoning,
                            content_hash,
                        }));
                        prev_text_end = text_end;
                        prev_reasoning_end = reason_end;
                    }
                }
                TurnSegment::Tool { tool_idx } => {
                    if let Some(t) = self.tool_cards.get(*tool_idx) {
                        let is_running = t.output_summary.is_none();
                        let running_duration_ms =
                            is_running.then(|| t.started_at.elapsed().as_millis() as u64);
                        // duration 按秒取整后纳入 hash——避免每毫秒 hash 变化导致
                        // 分片渲染缓存频繁失效；同时保证 duration 文本每秒刷新。
                        let duration_secs = running_duration_ms.map(|ms| ms / 1000);
                        vms.push(TuiRenderUnit::TuiToolCard(TuiToolCard {
                            tool_id: t.tool_id.clone(),
                            tool_name: t.tool_name.clone(),
                            input_summary: t.input_summary.clone(),
                            output_summary: t.output_summary.clone().unwrap_or_default(),
                            is_error: t.is_error,
                            is_running,
                            running_duration_ms,
                            diff: None,
                            tool_calls_count: 0,
                            content_hash: tui_hash_str(&format!(
                                "{}|{}|{}|{}|{}|{}|{:?}",
                                t.tool_id,
                                t.tool_name,
                                t.input_summary,
                                t.output_summary.as_deref().unwrap_or(""),
                                t.is_error,
                                is_running,
                                duration_secs,
                            )),
                        }));
                    }
                }
                TurnSegment::SubAgent { subagent_idx } => {
                    if let Some(s) = self.subagents.get(*subagent_idx) {
                        vms.push(s.view_model());
                    }
                }
                TurnSegment::SystemNote {
                    text,
                    level,
                    content_hash,
                } => {
                    vms.push(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                        text: text.clone(),
                        level: level.clone(),
                        content_hash: *content_hash,
                    }));
                }
            }
        }

        // Flush remaining content (after last segment, or if no segments exist)
        if prev_text_end < self.text.len() || prev_reasoning_end < self.reasoning.len() {
            let text_slice = &self.text[prev_text_end..];
            let reasoning_slice = &self.reasoning[prev_reasoning_end..];
            let (reasoning, content_hash) = build_reasoning(text_slice, reasoning_slice);
            vms.push(TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                text: text_slice.to_string(),
                reasoning,
                content_hash,
            }));
        }

        // 后处理：将 Agent 工具卡片的 tool_calls_count 与紧随的 SubAgent 组配对。
        // 匹配逻辑：TuiToolCard(tool_name="Agent") 紧接着 TuiSubAgentGroup。
        for i in 0..vms.len().saturating_sub(1) {
            if let (
                TuiRenderUnit::TuiToolCard(agent_card),
                TuiRenderUnit::TuiSubAgentGroup(subagent_group),
            ) = (&vms[i], &vms[i + 1])
                && agent_card.tool_name == "Agent"
                && agent_card.is_running
            {
                let tool_count = subagent_group
                    .view_models
                    .iter()
                    .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
                    .count();
                if tool_count > 0 {
                    let mut updated_card = agent_card.clone();
                    updated_card.tool_calls_count = tool_count;
                    vms[i] = TuiRenderUnit::TuiToolCard(updated_card);
                }
            }
        }

        self.cached_view_models = vms;
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

/// In-progress tool card accumulator.
#[derive(Debug, Clone)]
pub struct ToolCardAccumulator {
    /// Tool call identifier (matches `tool_id` from the protocol).
    pub tool_id: String,
    /// Human-readable tool name (e.g. `"Edit"`, `"Bash"`).
    pub tool_name: String,
    /// Short summary of the tool's input arguments.
    pub input_summary: String,
    /// Short summary of the tool's output (filled by `"tool-ended"`).
    pub output_summary: Option<String>,
    /// Whether the tool returned an error.
    pub is_error: bool,
    /// When the tool started on the TUI side.
    pub started_at: Instant,
    /// 是否已有 SubAgent segment 声明与此 ToolCard 关联。
    /// 防止多 Agent 场景下 SubAgent 段错配到错误的 ToolCard。
    pub claimed_by_subagent: bool,
}

impl ToolCardAccumulator {
    /// Create a new in-progress tool card from a `"tool-started"` payload.
    pub fn new(tool_id: String, tool_name: String, input_summary: String) -> Self {
        Self {
            tool_id,
            tool_name,
            input_summary,
            output_summary: None,
            is_error: false,
            started_at: Instant::now(),
            claimed_by_subagent: false,
        }
    }
}

/// In-progress sub-agent accumulator.
#[derive(Debug, Clone)]
pub struct SubAgentAccumulator {
    pub agent_id: String,
    pub agent_name: String,
    pub is_running: bool,
    pub child_turn: CurrentTurn,
    /// Cached view_model result, invalidated on any mutation.
    cached_view_model: std::cell::RefCell<Option<TuiRenderUnit>>,
}

impl SubAgentAccumulator {
    pub fn new(agent_id: String, agent_name: String) -> Self {
        Self {
            agent_id,
            agent_name,
            is_running: true,
            child_turn: CurrentTurn::new(),
            cached_view_model: std::cell::RefCell::new(None),
        }
    }

    fn append_text(&mut self, text: &str) {
        self.child_turn.append_text(text, None);
        self.cached_view_model.replace(None);
    }

    fn append_reasoning(&mut self, text: &str) {
        self.child_turn.append_reasoning(text, None);
        self.cached_view_model.replace(None);
    }

    fn start_tool(&mut self, tool: ToolCardAccumulator) {
        self.child_turn.start_tool(tool);
        self.cached_view_model.replace(None);
    }

    fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) {
        self.child_turn.end_tool(tool_id, output, is_error);
        self.cached_view_model.replace(None);
    }

    pub(crate) fn view_model(&self) -> TuiRenderUnit {
        // 缓存命中——直接返回，避免 child_turn.clone() + view_models() 全量重建
        if let Some(vm) = self.cached_view_model.borrow().as_ref() {
            return vm.clone();
        }

        let mut child_turn = self.child_turn.clone();
        let child_vms = child_turn.view_models();
        // M1: content_hash 累加每个 child VM 的 content_hash，确保 child 文本
        // 变化时（即使 child_vms.len() 不变）也能触发 render_bridge 增量重建。
        let child_content_hash: String = child_vms
            .iter()
            .map(|vm| vm.content_hash().to_string())
            .collect::<Vec<_>>()
            .join("|");
        let vm = TuiRenderUnit::TuiSubAgentGroup(crate::kit::tui_render_unit::TuiSubAgentGroup {
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            view_models: child_vms.iter().cloned().collect::<im::Vector<_>>(),
            collapsed: false,
            is_running: self.is_running,
            content_hash: crate::kit::tui_render_unit::tui_hash_str(&format!(
                "{}|{}|{}|{}|{}|{}",
                self.agent_id,
                self.agent_name,
                child_vms.len(),
                false,
                self.is_running,
                child_content_hash,
            )),
        });
        self.cached_view_model.replace(Some(vm.clone()));
        vm
    }
}

// ---------------------------------------------------------------------------
// AcpEventData -- decoded ACP custom event
// ---------------------------------------------------------------------------

/// AcpEventData + active_session_id 包装类型。
/// active_session_id 从 ACP 通知的 session_id 字段提取。
/// acp_bridge 消费时与 state.active_session_id 比较以丢弃陈旧滞留事件。
#[derive(Debug, Clone)]
pub struct AcpEventWithEpoch {
    pub event: AcpEventData,
    pub active_session_id: String,
}

/// Decoded ACP custom event.
///
/// One variant per event name defined in the ACP protocol section 4
/// ("Event Directory", see `docs/design/peri-acp-protocol.md`).
///
/// The [`decode`](AcpEventData::decode) method maps a raw `{event, data}`
/// payload to the corresponding typed variant. Unknown event names are
/// captured as [`AcpEventData::Unknown`] for forward compatibility.
#[derive(Debug, Clone)]
pub enum AcpEventData {
    // -- §4.1 Streaming (high-frequency) ------------------------------------
    /// `"text-chunk"` -- incremental text for the current assistant bubble.
    TextChunk(TuiTextChunk),

    /// `"reasoning-chunk"` -- incremental reasoning / thinking text.
    ReasoningChunk(TuiReasoningChunk),

    /// `"tool-started"` -- creates an in-progress tool card.
    ToolStarted(TuiToolStarted),

    /// `"tool-ended"` -- fills in the tool card result.
    ToolEnded(TuiToolEnded),

    // -- §4.2 Boundary (low-frequency) -------------------------------------
    /// 本地提交已进入当前 ACP session，开始一轮真实 agent turn。
    PromptStarted,

    /// TUI 内部事件：用户已提交 prompt，loading spinner 应立即显示。
    /// submit_consumer 发出，bridge 收到后设 phase=PromptRunning, variant=1。
    PromptSubmitted,

    /// session/load 历史恢复开始。Replay 不是 agent turn，不能触发 loading。
    SessionReplayStarted,

    /// session/load 历史恢复结束。
    SessionReplayDone,

    /// `"turn-done"` -- agent finished this turn (Streaming -> Idle).
    TurnDone,

    /// `"turn-interrupted"` -- agent was interrupted (user cancel / timeout).
    TurnInterrupted { reason: String },

    /// `"turn-suspended"` -- agent turn suspended, waiting for bg agent/cron/workflow.
    /// TUI 收到后应归档 current_turn + 停止 loading spinner。
    /// Agent 保持存活（await_wake），新 turn 的流事件自动恢复 loading。
    TurnSuspended,

    /// TUI 内部事件：本地用户提交的 UserBubble。仅 TUI 内部使用，不走 ACP 协议。
    LocalUserBubble { text: String },

    /// bg agent 完成回调 user bubble——要求先 flush current_turn 到 committed，
    /// 再 push 自身。与 LocalUserBubble 的纯追加不同，此变体主动切分视觉 turn：
    /// 在 agent ReAct 循环中间插入用户气泡，把同一轮 TurnDone 的 AI 内容
    /// 分割为「bg 回调前」和「bg 回调后」两段。
    BgCallbackBubble { text: String },

    /// TUI 内部事件：直接将完整 AI 文本气泡追加到 committed。
    /// 用于 session/load replay 及任何需要旁路 current_turn 直接归档的场景。
    /// `reasoning` 非空时会创建独立的 reasoning 折叠块。
    CommittedAssistantText {
        text: String,
        reasoning: Option<String>,
    },

    /// replay 工具调用开始——直接写入 committed 的 TuiToolCard（is_running=true）。
    ReplayToolStarted {
        tool_id: String,
        tool_name: String,
        input_summary: String,
    },

    /// replay 工具调用结束——更新 committed 中对应 tool_id 的 TuiToolCard。
    ReplayToolEnded {
        tool_id: String,
        output_summary: String,
        is_error: bool,
    },

    // -- §4.3 Status (status bar updates) ----------------------------------
    /// `"tool-count"` -- number of tool calls in the current turn.
    ToolCount(ToolCount),

    /// `"progress"` -- progress percentage with label.
    Progress(Progress),

    /// `"budget-warning"` -- context budget threshold crossed.
    BudgetWarning(BudgetWarning),

    /// `"system-notification"` -- system-level notification text with severity.
    SystemNotification(SystemNotification),

    // -- §4.4 Input assist -------------------------------------------------
    /// `"prediction"` -- input prediction suggestion (grey placeholder).
    Prediction(Prediction),

    /// `"file-suggestions"` -- @-mention file completion candidates.
    FileSuggestions(FileSuggestions),

    // -- §4.5 Interaction requests (require user decision) ------------------
    /// `"hitl-pending"` -- HITL tool approval request.
    HitlPending(HitlPending),

    /// `"ask-user"` -- multi-question form initiated by the agent.
    AskUser(AskUser),

    /// `"rewind-preview"` -- preview of changes that will be undone.
    RewindPreview(RewindPreview),

    /// Rewind 已完成——messages_json 为 BaseMessage 数组的 JSON。
    /// 由 AcpEvent::RewindCompleted（peri/agent_event）转换而来，
    /// dispatch_and_notify 反序列化后替换 state.committed。
    RewindCompleted { messages_json: String },

    /// `"oauth-needed"` -- MCP server authorization required.
    OauthNeeded(OauthNeeded),

    // -- §4.6 Structure (control message-area layout) ------------------------
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted {
        agent_id: String,
        agent_name: String,
        is_background: bool,
    },

    /// `"subagent-stopped"` -- sub-agent exited, TUI closes the group.
    SubagentStopped { agent_id: String },

    /// Fallback for unknown / future event names.
    ///
    /// Keeps the raw event name and JSON data so the state machine can log or
    /// silently ignore new events without crashing.
    Unknown {
        event: String,
        data: serde_json::Value,
    },

    // -- §4.7 Background Tasks (bg-task-*) ----------------------------------
    /// `"bg-task-started"` -- a background task has been registered.
    BgTaskStarted(BgTaskEntry),

    /// `"bg-task-completed"` -- a background task has finished.
    BgTaskCompleted {
        task_id: String,
        success: bool,
        duration_ms: u64,
    },

    /// `"bg-task-cancelled"` -- a background task was cancelled.
    BgTaskCancelled { task_id: String, reason: String },

    /// `"bg-task-snapshot"` -- full list of active background tasks.
    BgTaskSnapshot(Vec<BgTaskEntry>),

    // -- §4.8 Agent Event Extensions (P1-5) ----------------------------------
    /// `"turn-committed"` — ReAct 迭代提交信号。
    TurnCommitted { messages_json: String, steps: usize },

    /// `"compact-started"` — 上下文压缩开始。
    CompactStarted,

    /// `"compact-completed"` — 上下文压缩完成。
    CompactCompleted {
        summary: String,
        files: Vec<serde_json::Value>,
        skills: Vec<String>,
        micro_cleared: usize,
        messages_json: String,
    },

    /// `"compact-error"` — 上下文压缩失败。
    CompactError { message: String },

    /// `"background-task-completed"` — 后台 agent 任务完成。
    BackgroundTaskCompleted {
        task_id: String,
        agent_name: String,
        success: bool,
        output: String,
        tool_calls_count: usize,
        duration_ms: u64,
        child_thread_id: Option<String>,
    },

    /// `"agent-execution-failed"` — agent 执行失败。
    AgentExecutionFailed { message: String },

    /// `"workflow-progress"` — 工作流进度更新。
    WorkflowProgress {
        run_id: String,
        workflow_name: String,
        event_type: String,
        agent_id: Option<u64>,
        phase: Option<String>,
        label: Option<String>,
        agent_status: Option<String>,
        token_count: Option<u64>,
        tool_count: Option<u64>,
        run_status: Option<String>,
        message: Option<String>,
    },

    // -- §4.9 Plugin events ------------------------------------------------
    /// `"plugin-snapshot"` — 插件列表全量快照。
    PluginSnapshot(PluginSnapshot),
    /// `"plugin-action-result"` — 插件操作结果通知。
    PluginActionResult(PluginActionResult),
    /// `"plugin-search-result"` — Discover 搜索返回。
    PluginSearchResult(PluginSearchResult),
}

impl AcpEventData {
    /// Decode a raw `{event, data}` payload into a typed [`AcpEventData`].
    ///
    /// Dispatches by event name (kebab-case string). On deserialization
    /// failure or unknown event name, falls back to [`AcpEventData::Unknown`].
    pub fn decode(event: &str, data: serde_json::Value) -> Self {
        match event {
            // §4.1 Streaming -- deprecated, now delivered via session/update
            // "text-chunk", "reasoning-chunk", "tool-started", "tool-ended" 解码分支已移除。
            // 流式事件现在由 handle_session_update（acp_notifier.rs）处理。

            // §4.2 Boundary
            "turn-done" => AcpEventData::TurnDone,
            "turn-interrupted" => {
                let reason = data["reason"].as_str().unwrap_or("").to_string();
                AcpEventData::TurnInterrupted { reason }
            }
            "turn-suspended" => AcpEventData::TurnSuspended,

            // §4.3 Status
            "tool-count" => decode_or_unknown(event, data, AcpEventData::ToolCount),
            "progress" => decode_or_unknown(event, data, AcpEventData::Progress),
            "budget-warning" => decode_or_unknown(event, data, AcpEventData::BudgetWarning),
            "system-notification" => {
                decode_or_unknown(event, data, AcpEventData::SystemNotification)
            }

            // §4.4 Input assist
            "prediction" => decode_or_unknown(event, data, AcpEventData::Prediction),
            "file-suggestions" => decode_or_unknown(event, data, AcpEventData::FileSuggestions),

            // §4.5 Interaction requests
            "rewind-preview" => decode_or_unknown(event, data, AcpEventData::RewindPreview),
            "rewind-completed" => {
                let messages_json = data["messages_json"].as_str().unwrap_or("").to_string();
                AcpEventData::RewindCompleted { messages_json }
            }
            "oauth-needed" => decode_or_unknown(event, data, AcpEventData::OauthNeeded),

            // §4.6 Structure
            "subagent-started" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                let agent_name = data["agent_name"].as_str().unwrap_or("").to_string();
                let is_background = data["is_background"].as_bool().unwrap_or(false);
                AcpEventData::SubagentStarted {
                    agent_id,
                    agent_name,
                    is_background,
                }
            }
            "subagent-stopped" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                AcpEventData::SubagentStopped { agent_id }
            }

            // §4.7 Background Tasks
            "bg-task-started" => decode_or_unknown(event, data, AcpEventData::BgTaskStarted),
            "bg-task-completed" => decode_or_unknown(event, data, |d: BgTaskCompletedData| {
                AcpEventData::BgTaskCompleted {
                    task_id: d.task_id,
                    success: d.success,
                    duration_ms: d.duration_ms,
                }
            }),
            "bg-task-cancelled" => decode_or_unknown(event, data, |d: BgTaskCancelledData| {
                AcpEventData::BgTaskCancelled {
                    task_id: d.task_id,
                    reason: d.reason,
                }
            }),
            "bg-task-snapshot" => decode_or_unknown(event, data, AcpEventData::BgTaskSnapshot),

            "bg-callback-user-message" => {
                let text = data["text"].as_str().unwrap_or("").to_string();
                AcpEventData::BgCallbackBubble { text }
            }

            // -- §4.9 Plugin events ----------------------------------------
            "plugin-snapshot" => decode_or_unknown(event, data, AcpEventData::PluginSnapshot),
            "plugin-action-result" => {
                decode_or_unknown(event, data, AcpEventData::PluginActionResult)
            }
            "plugin-search-result" => {
                decode_or_unknown(event, data, AcpEventData::PluginSearchResult)
            }

            // Unknown / future event names -- forward-compatible fallback.
            _ => AcpEventData::unknown(event, data),
        }
    }

    /// Helper to construct the [`AcpEventData::Unknown`] variant.
    fn unknown(event: &str, data: serde_json::Value) -> Self {
        AcpEventData::Unknown {
            event: event.to_owned(),
            data,
        }
    }
}

// ---------------------------------------------------------------------------
// BgTaskEntry -- TUI-side background task entry
// ---------------------------------------------------------------------------

/// Background task entry mirroring `BgTaskInfo` from the agent layer.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BgTaskEntry {
    pub task_id: String,
    pub kind: String,
    pub summary: String,
    pub started_at: String,
    pub pid: Option<u32>,
}

/// Deserialization helper for `bg-task-completed` payload.
#[derive(Debug, serde::Deserialize)]
struct BgTaskCompletedData {
    task_id: String,
    success: bool,
    duration_ms: u64,
}

/// Deserialization helper for `bg-task-cancelled` payload.
#[derive(Debug, serde::Deserialize)]
struct BgTaskCancelledData {
    task_id: String,
    reason: String,
}

/// Decode `data` into `T` and apply the variant constructor, or fall back to
/// [`AcpEventData::Unknown`] with the original `data` preserved.
fn decode_or_unknown<T, F>(event: &str, data: serde_json::Value, ctor: F) -> AcpEventData
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> AcpEventData,
{
    match serde_json::from_value::<T>(data.clone()) {
        Ok(v) => ctor(v),
        Err(_) => AcpEventData::unknown(event, data),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- CurrentTurn tests ----------------------------------------------------

    #[test]
    fn test_default_empty() {
        let mut ct = CurrentTurn::default();
        assert!(ct.text.is_empty());
        assert!(ct.reasoning.is_empty());
        assert!(ct.tool_cards.is_empty());
        assert!(!ct.active);
        assert!(ct.view_models().is_empty());
    }

    #[test]
    fn test_new_equals_default() {
        let a = CurrentTurn::new();
        let b = CurrentTurn::default();
        assert_eq!(a.text, b.text);
        assert_eq!(a.active, b.active);
    }

    #[test]
    fn test_append_text_sets_active() {
        let mut ct = CurrentTurn::new();
        assert!(!ct.active);
        ct.append_text("hello ", None);
        ct.append_text("world", None);
        assert_eq!(ct.text, "hello world");
        assert!(ct.active);
    }

    #[test]
    fn test_append_reasoning_sets_active() {
        let mut ct = CurrentTurn::new();
        ct.append_reasoning("thinking...", None);
        assert_eq!(ct.reasoning, "thinking...");
        assert!(ct.active);
    }

    #[test]
    fn test_start_then_end_tool() {
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Edit".into(),
            "path: foo.rs".into(),
        ));
        assert_eq!(ct.tool_cards.len(), 1);
        assert!(ct.active);

        ct.end_tool("tc-1", "updated 3 lines".into(), false);
        let card = &ct.tool_cards[0];
        assert_eq!(card.output_summary.as_deref(), Some("updated 3 lines"));
        assert!(!card.is_error);
    }

    #[test]
    fn test_end_tool_unknown_id_is_noop() {
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Edit".into(),
            "x".into(),
        ));
        ct.end_tool("does-not-exist", "out".into(), true);
        assert!(ct.tool_cards[0].output_summary.is_none());
        assert!(!ct.tool_cards[0].is_error);
    }

    #[test]
    fn test_bash_timer_hash_changes_over_time() {
        // [设计变更] ToolCard content_hash 现在纳入 duration（按秒向下取整）——
        // 这是为了让按 hash 分片的渲染缓存每秒刷新一次 duration 文本。
        // 此测试验证：跨秒后 content_hash 变化（触发缓存失效 + duration 文本更新）。
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-bash".into(),
            "Bash".into(),
            "cargo test".into(),
        ));

        let first_hash = match &ct.view_models()[0] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert!(card.is_running);
                assert!(card.running_duration_ms.is_some());
                card.content_hash
            }
            other => panic!("expected TuiToolCard, got {other:?}"),
        };

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        ct.invalidate_cache();

        let second_hash = match &ct.view_models()[0] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert!(card.is_running);
                assert!(card.running_duration_ms.unwrap() >= 1_000);
                card.content_hash
            }
            other => panic!("expected TuiToolCard, got {other:?}"),
        };

        // 跨秒后 duration_secs 从 0 变为 1，content_hash 必须变化
        assert_ne!(
            first_hash, second_hash,
            "跨秒后 duration_secs 变化，content_hash 必须变化以触发缓存失效"
        );
    }

    #[test]
    fn test_completed_bash_hash_stays_same() {
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-bash".into(),
            "Bash".into(),
            "cargo test".into(),
        ));
        ct.end_tool("tc-bash", "ok".into(), false);

        let first_hash = match &ct.view_models()[0] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert!(!card.is_running);
                assert_eq!(card.running_duration_ms, None);
                card.content_hash
            }
            other => panic!("expected TuiToolCard, got {other:?}"),
        };

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        ct.invalidate_cache();

        let second_hash = match &ct.view_models()[0] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert!(!card.is_running);
                assert_eq!(card.running_duration_ms, None);
                card.content_hash
            }
            other => panic!("expected TuiToolCard, got {other:?}"),
        };

        assert_eq!(first_hash, second_hash);
    }

    #[test]
    fn test_deactivate() {
        let mut ct = CurrentTurn::new();
        ct.append_text("x", None);
        assert!(ct.active);
        ct.deactivate();
        assert!(!ct.active);
    }

    // -- AcpEventData decode tests -------------------------------------------

    #[test]
    fn test_current_turn_subagent_streaming_builds_nested_group() {
        let mut ct = CurrentTurn::new();
        ct.start_subagent("agent-1".into(), "researcher".into());
        assert!(ct.append_subagent_text("agent-1", "hello"));
        assert!(ct.start_subagent_tool(
            "agent-1",
            ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
        ));
        assert!(ct.end_subagent_tool("agent-1", "tc-1", "10 lines".into(), false));

        let vms = ct.view_models().to_vec();
        assert_eq!(vms.len(), 1);
        match &vms[0] {
            TuiRenderUnit::TuiSubAgentGroup(group) => {
                assert_eq!(group.agent_id, "agent-1");
                assert_eq!(group.agent_name, "researcher");
                assert_eq!(group.view_models.len(), 2);
            }
            other => panic!("expected TuiSubAgentGroup, got {other:?}"),
        }
    }

    #[test]
    fn test_current_turn_subagent_unknown_route_returns_false() {
        let mut ct = CurrentTurn::new();
        assert!(!ct.append_subagent_text("missing", "hello"));
        assert!(ct.view_models().is_empty());
    }

    #[test]
    fn test_decode_turn_done() {
        let decoded = AcpEventData::decode("turn-done", serde_json::json!({}));
        match decoded {
            AcpEventData::TurnDone => {}
            _ => panic!("expected TurnDone"),
        }
    }

    #[test]
    fn test_decode_turn_interrupted() {
        let data = serde_json::json!({"reason": "user cancelled"});
        let decoded = AcpEventData::decode("turn-interrupted", data);
        match decoded {
            AcpEventData::TurnInterrupted { reason } => assert_eq!(reason, "user cancelled"),
            _ => panic!("expected TurnInterrupted"),
        }
    }

    #[test]
    fn test_decode_tool_count() {
        let data = serde_json::json!({"count": 3});
        let decoded = AcpEventData::decode("tool-count", data);
        match decoded {
            AcpEventData::ToolCount(tc) => assert_eq!(tc.count, 3),
            _ => panic!("expected ToolCount"),
        }
    }

    #[test]
    fn test_decode_budget_warning() {
        let data = serde_json::json!({
            "used": 85000,
            "limit": 100000,
            "threshold": "0.85"
        });
        let decoded = AcpEventData::decode("budget-warning", data);
        match decoded {
            AcpEventData::BudgetWarning(bw) => assert_eq!(bw.threshold, "0.85"),
            _ => panic!("expected BudgetWarning"),
        }
    }

    #[test]
    fn test_decode_system_notification() {
        let data = serde_json::json!({"text": "model switched", "level": "info"});
        let decoded = AcpEventData::decode("system-notification", data);
        match decoded {
            AcpEventData::SystemNotification(sn) => assert_eq!(sn.level, "info"),
            _ => panic!("expected SystemNotification"),
        }
    }

    #[test]
    fn test_decode_prediction() {
        let data = serde_json::json!({"text": "fix typo"});
        let decoded = AcpEventData::decode("prediction", data);
        match decoded {
            AcpEventData::Prediction(p) => assert_eq!(p.text, "fix typo"),
            _ => panic!("expected Prediction"),
        }
    }

    #[test]
    fn test_decode_file_suggestions() {
        let data = serde_json::json!({"files": ["src/main.rs", "src/lib.rs"]});
        let decoded = AcpEventData::decode("file-suggestions", data);
        match decoded {
            AcpEventData::FileSuggestions(fs) => assert_eq!(fs.files.len(), 2),
            _ => panic!("expected FileSuggestions"),
        }
    }

    #[test]
    fn test_decode_rewind_preview() {
        let data = serde_json::json!({"files": [], "messages": []});
        let decoded = AcpEventData::decode("rewind-preview", data);
        match decoded {
            AcpEventData::RewindPreview(rp) => assert!(rp.files.is_empty()),
            _ => panic!("expected RewindPreview"),
        }
    }

    #[test]
    fn test_decode_oauth_needed() {
        let data = serde_json::json!({
            "server_name": "github-mcp",
            "auth_url": "https://github.com/login/oauth"
        });
        let decoded = AcpEventData::decode("oauth-needed", data);
        match decoded {
            AcpEventData::OauthNeeded(on) => assert_eq!(on.server_name, "github-mcp"),
            _ => panic!("expected OauthNeeded"),
        }
    }

    #[test]
    fn test_decode_subagent_started() {
        let data = serde_json::json!({
            "agent_id": "sa-1",
            "agent_name": "file-searcher"
        });
        let decoded = AcpEventData::decode("subagent-started", data);
        match decoded {
            AcpEventData::SubagentStarted { agent_name, .. } => {
                assert_eq!(agent_name, "file-searcher")
            }
            _ => panic!("expected SubagentStarted"),
        }
    }

    #[test]
    fn test_decode_subagent_stopped() {
        let data = serde_json::json!({"agent_id": "sa-1"});
        let decoded = AcpEventData::decode("subagent-stopped", data);
        match decoded {
            AcpEventData::SubagentStopped { agent_id } => assert_eq!(agent_id, "sa-1"),
            _ => panic!("expected SubagentStopped"),
        }
    }

    #[test]
    fn test_decode_unknown_event_name() {
        let data = serde_json::json!({"foo": "bar"});
        let decoded = AcpEventData::decode("future-event", data);
        match decoded {
            AcpEventData::Unknown { event, data } => {
                assert_eq!(event, "future-event");
                assert_eq!(data["foo"], "bar");
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn test_decode_malformed_data_falls_to_unknown() {
        let data = serde_json::json!("not an object");
        let decoded = AcpEventData::decode("future-event-xyz", data);
        match decoded {
            AcpEventData::Unknown { event, .. } => assert_eq!(event, "future-event-xyz"),
            _ => panic!("expected Unknown for malformed data"),
        }
    }

    // ── Segment interleaving tests ─────────────────────────────────────────

    /// 工具调用之间由 message_id 变化驱动的文本段分隔。
    ///
    /// 场景：Agent 说"1"（message_A）→ Read → 说"2"（message_B）→ Bash。
    /// 期望 view_models 产出 4 项，顺序为
    /// [TuiAssistantBubble("1"), TuiToolCard(Read), TuiAssistantBubble("2"), TuiToolCard(Bash)]
    #[test]
    fn test_build_view_models_interleaves_text_and_tools() {
        let mut ct = CurrentTurn::new();
        ct.append_text("1", Some("msg_A"));
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Read".into(),
            "file: a.rs".into(),
        ));
        ct.end_tool("tc-1", "ok".into(), false);
        ct.append_text("2", Some("msg_B"));
        ct.start_tool(ToolCardAccumulator::new(
            "tc-2".into(),
            "Bash".into(),
            "echo hi".into(),
        ));
        ct.end_tool("tc-2", "hi".into(), false);

        let vms: Vec<_> = ct.view_models().to_vec();
        assert_eq!(vms.len(), 4, "应为 4 项：Text→Tool→Text→Tool");
        assert!(
            matches!(&vms[0], TuiRenderUnit::TuiAssistantBubble(_)),
            "[0] 应为 Text bubble (1)"
        );
        assert!(
            matches!(&vms[1], TuiRenderUnit::TuiToolCard(_)),
            "[1] 应为 Tool card (Read)"
        );
        assert!(
            matches!(&vms[2], TuiRenderUnit::TuiAssistantBubble(_)),
            "[2] 应为 Text bubble (2)"
        );
        assert!(
            matches!(&vms[3], TuiRenderUnit::TuiToolCard(_)),
            "[3] 应为 Tool card (Bash)"
        );

        // 验证文本内容是否正确分离（不是整体拼接）
        match &vms[0] {
            TuiRenderUnit::TuiAssistantBubble(b) => assert_eq!(b.text, "1"),
            _ => unreachable!(),
        }
        match &vms[2] {
            TuiRenderUnit::TuiAssistantBubble(b) => assert_eq!(b.text, "2"),
            _ => unreachable!(),
        }
    }

    /// 同一 message_id 的多段文本不拆开，保持为一个 bubble。
    #[test]
    fn test_same_message_id_keeps_text_contiguous() {
        let mut ct = CurrentTurn::new();
        ct.append_text("part1", Some("msg_A"));
        ct.append_text(" part2", Some("msg_A"));
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Read".into(),
            "f: x.rs".into(),
        ));

        let vms: Vec<_> = ct.view_models().to_vec();
        assert_eq!(vms.len(), 2, "1 个 Text bubble + 1 个 Tool card");
        match &vms[0] {
            TuiRenderUnit::TuiAssistantBubble(b) => {
                assert_eq!(b.text, "part1 part2", "同 message_id 不应拆分");
            }
            _ => panic!("[0] 应为 Text bubble"),
        }
    }

    /// 无 message_id（旧事件或协议不携带）时，依赖 tool/subagent 边界分段。
    #[test]
    fn test_no_message_id_uses_tool_boundaries() {
        let mut ct = CurrentTurn::new();
        ct.append_text("a", None);
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Read".into(),
            "f: x.rs".into(),
        ));
        ct.end_tool("tc-1", "ok".into(), false);
        ct.append_text("b", None);

        let vms: Vec<_> = ct.view_models().to_vec();
        assert_eq!(vms.len(), 3, "Text→Tool→Text");
        assert!(matches!(&vms[0], TuiRenderUnit::TuiAssistantBubble(_)));
        assert!(matches!(&vms[1], TuiRenderUnit::TuiToolCard(_)));
        assert!(matches!(&vms[2], TuiRenderUnit::TuiAssistantBubble(_)));
    }

    /// M1: SubAgentAccumulator content_hash 随 child VM 内容变化。
    /// 相同结构（1 个 child）但不同文本 → 不同 content_hash。
    #[test]
    fn test_subagent_content_hash_changes_with_child_content() {
        let mut acc1 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
        acc1.append_text("hello");
        let vm1 = acc1.view_model();
        let hash1 = match &vm1 {
            TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
            _ => panic!("expected TuiSubAgentGroup"),
        };

        let mut acc2 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
        acc2.append_text("world");
        let vm2 = acc2.view_model();
        let hash2 = match &vm2 {
            TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
            _ => panic!("expected TuiSubAgentGroup"),
        };

        assert_ne!(
            hash1, hash2,
            "不同 child 内容应产出不同 content_hash（M1 修复前会相等）"
        );
    }

    /// [回归测试] 每个 batch 的第一个工具调用应在完成后 is_running=false。
    ///
    /// 场景复现 issue #2026-07-20-first-tool-call-per-batch-stuck-running：
    /// reasoning → tool1 启动 → 更多 reasoning → tool2 启动 →
    /// tool1 结束 → tool2 结束。
    /// 预期两个工具完成后 is_running 都为 false。
    #[test]
    fn test_first_tool_in_batch_is_running_false_after_end() {
        let mut ct = CurrentTurn::new();

        // 第一批 reasoning
        ct.append_reasoning("思考了 653 字符...", None);
        // 第一个工具启动
        ct.start_tool(ToolCardAccumulator::new(
            "tc-shell-1".into(),
            "Shell".into(),
            "git log --oneline -15".into(),
        ));
        // 第二批 reasoning（在工具 1 启动后到达）
        ct.append_reasoning("思考了 302 字符...", None);
        // 第二个工具启动
        ct.start_tool(ToolCardAccumulator::new(
            "tc-shell-2".into(),
            "Shell".into(),
            "git show --stat e5239171".into(),
        ));
        // 第一个工具结束
        ct.end_tool("tc-shell-1", "c4596722 refactor...".into(), false);
        // 第二个工具结束
        ct.end_tool("tc-shell-2", "commit e5239171...".into(), false);

        let vms: Vec<_> = ct.view_models().to_vec();

        // 期望：2 个 reasoning bubble + 2 个 tool card = 4 个 VM
        assert_eq!(vms.len(), 4, "应为 2 个 AssistantBubble + 2 个 ToolCard");

        // 验证第一个工具卡片：is_running 应为 false
        match &vms[1] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert_eq!(card.tool_id, "tc-shell-1");
                assert!(
                    !card.is_running,
                    "[回归测试] 第一个工具调用完成后的 is_running 应为 false，实际为 true"
                );
                assert!(
                    !card.output_summary.is_empty(),
                    "第一个工具完成后的 output_summary 不应为空"
                );
            }
            _ => panic!("vms[1] 应为 TuiToolCard"),
        }

        // 验证第二个工具卡片：is_running 也应为 false
        match &vms[3] {
            TuiRenderUnit::TuiToolCard(card) => {
                assert_eq!(card.tool_id, "tc-shell-2");
                assert!(
                    !card.is_running,
                    "第二个工具调用完成后的 is_running 也应为 false"
                );
                assert!(!card.output_summary.is_empty());
            }
            _ => panic!("vms[3] 应为 TuiToolCard"),
        }
    }
}
