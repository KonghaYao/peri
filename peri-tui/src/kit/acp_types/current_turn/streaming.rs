use super::super::tool_card::ToolCardAccumulator;
use super::{CurrentTurn, TurnSegment};
use crate::kit::tui_render_unit::{TuiNoteLevel, tui_hash_roll_update};
use std::time::Instant;

impl CurrentTurn {
    /// If text has grown since the last `AssistantText` segment, push a new
    /// segment capturing the delta (both text and reasoning boundaries).
    ///
    /// 冻结时把当前 open 区域的滚动哈希存入段记录——此后该段内容不再变化，
    /// 缓存重建可直接 O(1) 取用，无需对冻结段重新哈希。
    /// 推理时长同刻冻结（§6.3 `Thought for Ns`）——flush 后不再增长。
    pub(super) fn flush_text_segment(&mut self) {
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
}
