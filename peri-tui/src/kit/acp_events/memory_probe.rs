//! TUI bridge 对象计量（只读诊断）。
//!
//! 口径与 peri-agent 的 `memory_probe` 一致：`body` = 已使用正文/内联槽位字节；
//! `reserved` = `String`/`Vec` capacity 下界。`im::Vector` 的共享节点与分配器
//! 元数据不计入；每层按分配地址去重，跨层 `unique_*` 共用同一去重集合。
//! 这些数字不改变任何释放/折叠语义。

use super::BridgeState;
use crate::kit::atoms::{BG_LIVE_DETAIL, FOLD_OVERRIDES};
use crate::kit::bg_task_live::BgLiveMemoryBytes;
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiCollapsedGroup, TuiDivider, TuiRenderUnit, TuiSubAgentGroup,
    TuiSystemReminder, TuiToolCard, TuiToolPresentation,
};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LayerBytes {
    body: usize,
    reserved: usize,
}

impl std::ops::Add for LayerBytes {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            body: self.body + rhs.body,
            reserved: self.reserved + rhs.reserved,
        }
    }
}

impl std::ops::AddAssign for LayerBytes {
    fn add_assign(&mut self, rhs: Self) {
        self.body += rhs.body;
        self.reserved += rhs.reserved;
    }
}

fn string_layer_bytes(value: &String) -> LayerBytes {
    LayerBytes {
        body: value.len(),
        reserved: value.capacity(),
    }
}

fn option_string_layer_bytes(value: &Option<String>) -> LayerBytes {
    value.as_ref().map(string_layer_bytes).unwrap_or_default()
}

fn vec_slot_layer_bytes<T>(values: &Vec<T>) -> LayerBytes {
    let element = std::mem::size_of::<T>();
    LayerBytes {
        body: values.len() * element,
        reserved: values.capacity() * element,
    }
}

/// `im::Vector` 不暴露容量；只报告已用槽位，共享节点不计入。
fn im_vector_slot_layer_bytes<T: Clone>(values: &im::Vector<T>) -> LayerBytes {
    let element = std::mem::size_of::<T>();
    LayerBytes {
        body: values.len() * element,
        reserved: values.len() * element,
    }
}

fn tool_presentation_bytes(presentation: &TuiToolPresentation) -> LayerBytes {
    match presentation {
        TuiToolPresentation::Generic => LayerBytes::default(),
        TuiToolPresentation::Skill(skill) => string_layer_bytes(&skill.name),
        TuiToolPresentation::Todo(todo) => {
            let mut bytes =
                vec_slot_layer_bytes(&todo.current_items) + vec_slot_layer_bytes(&todo.changes);
            for item in &todo.current_items {
                bytes += string_layer_bytes(&item.content);
                bytes += option_string_layer_bytes(&item.active_form);
            }
            for change in &todo.changes {
                bytes += string_layer_bytes(&change.content);
            }
            bytes
        }
    }
}

fn diff_layer_bytes(diff: &Option<crate::kit::tui_render_unit::TuiDiffBlock>) -> LayerBytes {
    let Some(diff) = diff else {
        return LayerBytes::default();
    };
    let mut bytes = string_layer_bytes(&diff.path) + vec_slot_layer_bytes(&diff.hunks);
    for hunk in &diff.hunks {
        bytes += string_layer_bytes(&hunk.old_range);
        bytes += string_layer_bytes(&hunk.new_range);
        bytes += vec_slot_layer_bytes(&hunk.lines);
        for line in &hunk.lines {
            bytes += string_layer_bytes(&line.text);
        }
    }
    bytes
}

fn measure_unit(
    unit: &TuiRenderUnit,
    seen: &mut HashSet<usize>,
    dedup_hits: &mut usize,
) -> LayerBytes {
    // 按分配身份去重：committed 与 folded_history 共享 im 节点时只计一次；
    // 同一 Arc bubble 重复出现时也只计一次。Arc 的槽位地址每次 clone 都不同，
    // 必须用 payload 指针，否则跨层 unique_* 会重复计入共享正文。
    let identity = match unit {
        TuiRenderUnit::TuiAssistantBubble(arc) => std::sync::Arc::as_ptr(arc) as usize,
        _ => unit as *const TuiRenderUnit as usize,
    };
    if !seen.insert(identity) {
        *dedup_hits += 1;
        return LayerBytes::default();
    }
    match unit {
        TuiRenderUnit::TuiUserBubble(bubble) => string_layer_bytes(&bubble.text),
        TuiRenderUnit::TuiAssistantBubble(arc) => {
            let mut bytes = string_layer_bytes(&arc.text);
            if let Some(reasoning) = &arc.reasoning {
                bytes += string_layer_bytes(&reasoning.text);
            }
            bytes += option_string_layer_bytes(&arc.message_id);
            bytes
        }
        TuiRenderUnit::TuiToolCard(card) => tool_card_bytes(card),
        TuiRenderUnit::TuiSystemNote(note) => string_layer_bytes(&note.text),
        TuiRenderUnit::TuiSystemReminder(reminder) => system_reminder_bytes(reminder),
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            let mut bytes = subagent_group_bytes(group);
            for child in &group.view_models {
                bytes += measure_unit(child, seen, dedup_hits);
            }
            bytes
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => {
            let mut bytes = collapsed_group_bytes(group);
            for child in &group.view_models {
                bytes += measure_unit(child, seen, dedup_hits);
            }
            bytes
        }
        TuiRenderUnit::TuiDivider(divider) => divider_bytes(divider),
        TuiRenderUnit::TuiAskUserBlock(block) => ask_user_bytes(block),
        TuiRenderUnit::TuiTodoSummary(summary) => string_layer_bytes(&summary.text),
    }
}

fn tool_card_bytes(card: &TuiToolCard) -> LayerBytes {
    string_layer_bytes(&card.tool_id)
        + string_layer_bytes(&card.tool_name)
        + string_layer_bytes(&card.input_summary)
        + string_layer_bytes(&card.output_summary)
        + tool_presentation_bytes(&card.presentation)
        + diff_layer_bytes(&card.diff)
}

fn system_reminder_bytes(reminder: &TuiSystemReminder) -> LayerBytes {
    string_layer_bytes(&reminder.summary)
        + string_layer_bytes(&reminder.body)
        + string_layer_bytes(&reminder.category)
        + string_layer_bytes(&reminder.source)
}

fn subagent_group_bytes(group: &TuiSubAgentGroup) -> LayerBytes {
    string_layer_bytes(&group.instance_id)
        + string_layer_bytes(&group.agent_id)
        + string_layer_bytes(&group.agent_name)
        + option_string_layer_bytes(&group.error_reason)
        + im_vector_slot_layer_bytes(&group.view_models)
}

fn collapsed_group_bytes(group: &TuiCollapsedGroup) -> LayerBytes {
    string_layer_bytes(&group.title) + vec_slot_layer_bytes(&group.view_models)
}

fn divider_bytes(divider: &TuiDivider) -> LayerBytes {
    option_string_layer_bytes(&divider.label)
}

fn ask_user_bytes(block: &TuiAskUserBlock) -> LayerBytes {
    let mut bytes = string_layer_bytes(&block.verb)
        + string_layer_bytes(&block.question)
        + option_string_layer_bytes(&block.result)
        + option_string_layer_bytes(&block.request_id)
        + vec_slot_layer_bytes(&block.options)
        + vec_slot_layer_bytes(&block.items)
        + vec_slot_layer_bytes(&block.question_ids);
    for option in &block.options {
        bytes += string_layer_bytes(option);
    }
    for item in &block.items {
        bytes += string_layer_bytes(&item.header);
        bytes += string_layer_bytes(&item.answer);
    }
    for id in &block.question_ids {
        bytes += string_layer_bytes(id);
    }
    bytes
}

fn measure_layer<'a>(
    units: impl IntoIterator<Item = &'a TuiRenderUnit>,
    seen: &mut HashSet<usize>,
    dedup_hits: &mut usize,
) -> LayerBytes {
    let mut bytes = LayerBytes::default();
    for unit in units {
        bytes += measure_unit(unit, seen, dedup_hits);
    }
    bytes
}

/// TUI bridge 驻留对象的只读快照。
///
/// `committed` / `current_turn` / `folded_history` 分层报告条目与字节；
/// `unique_*` 跨层按分配地址去重（非共享节点无法按分配身份识别，仍按出现
/// 次数计入）。这些数字不是分配器已回收量，也不改变释放语义。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BridgeMemorySnapshot {
    pub(crate) committed_entries: usize,
    pub(crate) committed_body_bytes: usize,
    pub(crate) committed_reserved_bytes: usize,
    pub(crate) current_turn_entries: usize,
    pub(crate) current_turn_body_bytes: usize,
    pub(crate) current_turn_reserved_bytes: usize,
    /// 原始累积器（text/reasoning/raw_input/presentation/child_turn 递归），
    /// 不含 `cached_view_models` 投影。
    pub(crate) current_turn_raw_body_bytes: usize,
    pub(crate) current_turn_raw_reserved_bytes: usize,
    pub(crate) folded_entries: usize,
    pub(crate) folded_body_bytes: usize,
    pub(crate) folded_reserved_bytes: usize,
    pub(crate) unique_body_bytes: usize,
    pub(crate) unique_reserved_bytes: usize,
    pub(crate) deduped_allocations: usize,
    /// `BG_LIVE_DETAIL` 的分层驻留计量。
    pub(crate) bg_live_entries: usize,
    pub(crate) bg_live_body_bytes: usize,
    pub(crate) bg_live_reserved_bytes: usize,
    pub(crate) bg_live_summary_bytes: usize,
    pub(crate) bg_live_output_preview_bytes: usize,
    pub(crate) bg_live_subagent_result_bytes: usize,
    pub(crate) bg_live_nested_units_bytes: usize,
    pub(crate) bg_live_stream_bytes: usize,
    pub(crate) bg_live_tool_cards_bytes: usize,
}

impl BridgeState {
    /// 只读诊断快照（测试与 debug 采样共用）。
    ///
    /// 需要投影 `folded_history`：`project` 在源/折叠状态未变化时是缓存命中，
    /// 只返回共享快照；它可能刷新 folded 缓存，但不淘汰、不修改
    /// committed/current_turn。
    pub(crate) fn memory_snapshot(&mut self) -> BridgeMemorySnapshot {
        let current_units = self.current_turn.view_models().clone();
        let current_entries = current_units.len();
        let folded_units = {
            let overrides_state = FOLD_OVERRIDES.state();
            let overrides = overrides_state.read();
            self.folded_history
                .project(&self.committed, self.phase, &overrides)
        };
        let folded_entries = folded_units.len();

        let mut committed_seen = HashSet::new();
        let mut current_seen = HashSet::new();
        let mut folded_seen = HashSet::new();
        let mut layer_dedup = 0usize;
        let committed = measure_layer(&self.committed, &mut committed_seen, &mut layer_dedup);
        let current = measure_layer(&current_units, &mut current_seen, &mut layer_dedup);
        let folded = measure_layer(&folded_units, &mut folded_seen, &mut layer_dedup);

        let mut unique_seen = HashSet::new();
        let mut unique_dedup = 0usize;
        let mut unique = measure_layer(&self.committed, &mut unique_seen, &mut unique_dedup);
        unique += measure_layer(&current_units, &mut unique_seen, &mut unique_dedup);
        unique += measure_layer(&folded_units, &mut unique_seen, &mut unique_dedup);

        let raw = self.current_turn.raw_memory_bytes();
        let mut bg_live = BgLiveMemoryBytes::default();
        let bg_live_entries = {
            let live = BG_LIVE_DETAIL.state();
            let map = live.read();
            for detail in map.values() {
                let bytes = detail.memory_bytes();
                bg_live.summary_bytes += bytes.summary_bytes;
                bg_live.output_preview_bytes += bytes.output_preview_bytes;
                bg_live.subagent_result_bytes += bytes.subagent_result_bytes;
                bg_live.nested_units_bytes += bytes.nested_units_bytes;
                bg_live.stream_bytes += bytes.stream_bytes;
                bg_live.tool_cards_bytes += bytes.tool_cards_bytes;
                bg_live.body += bytes.body;
                bg_live.reserved += bytes.reserved;
            }
            map.len()
        };

        BridgeMemorySnapshot {
            committed_entries: self.committed.len(),
            committed_body_bytes: committed.body,
            committed_reserved_bytes: committed.reserved,
            current_turn_entries: current_entries,
            current_turn_body_bytes: current.body,
            current_turn_reserved_bytes: current.reserved,
            current_turn_raw_body_bytes: raw.body,
            current_turn_raw_reserved_bytes: raw.reserved,
            folded_entries,
            folded_body_bytes: folded.body,
            folded_reserved_bytes: folded.reserved,
            unique_body_bytes: unique.body,
            unique_reserved_bytes: unique.reserved,
            deduped_allocations: unique_dedup + layer_dedup,
            bg_live_entries,
            bg_live_body_bytes: bg_live.body,
            bg_live_reserved_bytes: bg_live.reserved,
            bg_live_summary_bytes: bg_live.summary_bytes,
            bg_live_output_preview_bytes: bg_live.output_preview_bytes,
            bg_live_subagent_result_bytes: bg_live.subagent_result_bytes,
            bg_live_nested_units_bytes: bg_live.nested_units_bytes,
            bg_live_stream_bytes: bg_live.stream_bytes,
            bg_live_tool_cards_bytes: bg_live.tool_cards_bytes,
        }
    }

    /// DEBUG 未启用时零成本；输出结构化字段，不输出正文。
    pub(super) fn sample_memory(&mut self, stage: &'static str) {
        if !tracing::enabled!(target: "peri_memory", tracing::Level::DEBUG) {
            return;
        }
        let snapshot = self.memory_snapshot();
        // 次要证据：进程级/分配器统计与对象字节分开记录，不能互相替代。
        let allocator = crate::alloc_config::query_stats();
        tracing::debug!(
            target: "peri_memory",
            stage,
            origin = %self.active_session_id,
            committed_entries = snapshot.committed_entries,
            committed_body_bytes = snapshot.committed_body_bytes,
            committed_reserved_bytes = snapshot.committed_reserved_bytes,
            current_turn_entries = snapshot.current_turn_entries,
            current_turn_body_bytes = snapshot.current_turn_body_bytes,
            current_turn_reserved_bytes = snapshot.current_turn_reserved_bytes,
            current_turn_raw_body_bytes = snapshot.current_turn_raw_body_bytes,
            current_turn_raw_reserved_bytes = snapshot.current_turn_raw_reserved_bytes,
            folded_entries = snapshot.folded_entries,
            folded_body_bytes = snapshot.folded_body_bytes,
            folded_reserved_bytes = snapshot.folded_reserved_bytes,
            unique_body_bytes = snapshot.unique_body_bytes,
            unique_reserved_bytes = snapshot.unique_reserved_bytes,
            deduped_allocations = snapshot.deduped_allocations,
            bg_live_entries = snapshot.bg_live_entries,
            bg_live_body_bytes = snapshot.bg_live_body_bytes,
            bg_live_reserved_bytes = snapshot.bg_live_reserved_bytes,
            bg_live_summary_bytes = snapshot.bg_live_summary_bytes,
            bg_live_output_preview_bytes = snapshot.bg_live_output_preview_bytes,
            bg_live_subagent_result_bytes = snapshot.bg_live_subagent_result_bytes,
            bg_live_nested_units_bytes = snapshot.bg_live_nested_units_bytes,
            bg_live_stream_bytes = snapshot.bg_live_stream_bytes,
            bg_live_tool_cards_bytes = snapshot.bg_live_tool_cards_bytes,
            rss_bytes = allocator.map(|stats| stats.current_rss),
            allocator_allocated_bytes = allocator.map(|stats| stats.current_allocated),
            "object-memory tui bridge"
        );
    }
}
