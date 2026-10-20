//! 前缀归档：把第一个仍会增长的工具/子 Agent 之前的稳定内容交给 committed。
//!
//! 运行中的实体不能在此时归档：`committed` 中的工具卡片只有 replay 路径会更新，
//! 而 live 结束事件只写本容器；子 Agent 分组同理。因此切点取“最长稳定前缀”，
//! 而不是无条件把第一个运行中 SubAgent 之前的内容全部搬走。

use super::{CurrentTurn, TurnSegment};
use crate::kit::tui_render_unit::TuiRenderUnit;

impl CurrentTurn {
    /// 可安全归档的前缀段数。
    ///
    /// 遇到以下段立即停止（该段及其后内容保留在本容器）：
    /// - 仍在运行的工具卡片（后续 `tool-ended` 仍会更新它）；
    /// - 仍在运行的子 Agent（`subagent-stopped` 尚未到达）；
    /// - 尚未与父 Agent 卡片配对的子分组（迟到的卡片会把它插入到卡片之后，
    ///   提前归档会固定错误顺序）。
    pub(crate) fn archivable_prefix_len(&self) -> usize {
        for (index, segment) in self.segments.iter().enumerate() {
            match segment {
                TurnSegment::Tool { tool_idx } => match self.tool_cards.get(*tool_idx) {
                    Some(card) if card.output_summary.is_some() => {}
                    _ => return index,
                },
                TurnSegment::SubAgent { subagent_idx } => {
                    let Some(subagent) = self.subagents.get(*subagent_idx) else {
                        return index;
                    };
                    let pending = self
                        .pending_subagent_groups
                        .iter()
                        .any(|group| group.subagent_idx == *subagent_idx);
                    if subagent.is_running || pending {
                        return index;
                    }
                }
                _ => {}
            }
        }
        self.segments.len()
    }

    /// 取出稳定前缀对应的 VM（按段顺序），并重基准剩余内容。
    ///
    /// 归档后：
    /// - `text` / `reasoning` 去掉前缀字节，剩余段偏移同步减掉；
    /// - 工具卡片与子 Agent 容器只保留剩余段和待配对分组引用到的条目，
    ///   索引重映射；
    /// - 缓存清空，下一次 `view_models()` 从剩余段重建。
    ///
    /// 返回空表示没有可归档内容。
    pub(crate) fn take_archivable_prefix(&mut self) -> Vec<TuiRenderUnit> {
        let cut = self.archivable_prefix_len();
        if cut == 0 {
            return Vec::new();
        }

        // projection barrier：保证 cache[i] 与 segments[i] 对齐后取前缀。
        let prefix: Vec<TuiRenderUnit> = {
            let cached = self.view_models();
            cached.iter().take(cut).cloned().collect()
        };

        let mut text_len = 0usize;
        let mut reasoning_len = 0usize;
        for segment in &self.segments[..cut] {
            if let TurnSegment::AssistantText {
                text_end_byte,
                reasoning_end_byte,
                ..
            } = segment
            {
                text_len = text_len.max((*text_end_byte).min(self.text.len()));
                reasoning_len = reasoning_len.max((*reasoning_end_byte).min(self.reasoning.len()));
            }
        }

        // 剩余段引用的容器条目——只保留它们，避免已完成正文继续挂在容器上。
        let mut tool_remap = vec![None; self.tool_cards.len()];
        let mut next_tool = 0usize;
        for segment in &self.segments[cut..] {
            if let TurnSegment::Tool { tool_idx } = segment
                && tool_remap[*tool_idx].is_none()
            {
                tool_remap[*tool_idx] = Some(next_tool);
                next_tool += 1;
            }
        }
        let mut subagent_remap = vec![None; self.subagents.len()];
        let mut next_subagent = 0usize;
        for segment in &self.segments[cut..] {
            if let TurnSegment::SubAgent { subagent_idx } = segment
                && subagent_remap[*subagent_idx].is_none()
            {
                subagent_remap[*subagent_idx] = Some(next_subagent);
                next_subagent += 1;
            }
        }
        for group in &self.pending_subagent_groups {
            if let Some(slot) = subagent_remap.get_mut(group.subagent_idx)
                && slot.is_none()
            {
                *slot = Some(next_subagent);
                next_subagent += 1;
            }
        }

        self.text.drain(..text_len);
        self.reasoning.drain(..reasoning_len);
        self.segments.drain(..cut);
        for segment in &mut self.segments {
            match segment {
                TurnSegment::AssistantText {
                    text_end_byte,
                    reasoning_end_byte,
                    ..
                } => {
                    *text_end_byte = text_end_byte.saturating_sub(text_len);
                    *reasoning_end_byte = reasoning_end_byte.saturating_sub(reasoning_len);
                }
                TurnSegment::Tool { tool_idx } => {
                    *tool_idx = tool_remap[*tool_idx].expect("remaining segment keeps its tool");
                }
                TurnSegment::SubAgent { subagent_idx } => {
                    *subagent_idx =
                        subagent_remap[*subagent_idx].expect("remaining segment keeps its group");
                }
                _ => {}
            }
        }
        let mut kept_tools: Vec<Option<_>> = (0..next_tool).map(|_| None).collect();
        for (index, card) in std::mem::take(&mut self.tool_cards).into_iter().enumerate() {
            if let Some(new_index) = tool_remap[index] {
                kept_tools[new_index] = Some(card);
            }
        }
        self.tool_cards = kept_tools.into_iter().flatten().collect();
        let mut kept_subagents: Vec<Option<_>> = (0..next_subagent).map(|_| None).collect();
        for (index, subagent) in std::mem::take(&mut self.subagents).into_iter().enumerate() {
            if let Some(new_index) = subagent_remap[index] {
                kept_subagents[new_index] = Some(subagent);
            }
        }
        self.subagents = kept_subagents.into_iter().flatten().collect();
        self.pending_subagent_groups = std::mem::take(&mut self.pending_subagent_groups)
            .into_iter()
            .filter_map(|group| {
                subagent_remap[group.subagent_idx].map(|subagent_idx| super::PendingSubagentGroup {
                    subagent_idx,
                    parent_tool_call_id: group.parent_tool_call_id,
                })
            })
            .collect();
        self.last_text_flush = self.last_text_flush.saturating_sub(text_len);
        self.last_reasoning_flush = self.last_reasoning_flush.saturating_sub(reasoning_len);
        self.cached_view_models = im::Vector::new();
        self.cache_dirty = true;

        prefix
    }
}

#[cfg(test)]
#[path = "archive_test.rs"]
mod tests;
