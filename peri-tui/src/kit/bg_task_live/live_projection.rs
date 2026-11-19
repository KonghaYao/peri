//! `BG_LIVE_DETAIL` 投影写入。

use super::preview;

use crate::kit::acp_types::ToolCardAccumulator;

use crate::kit::tui_render_unit::EntryStatus;
use crate::kit::tui_render_unit::FoldState;
use crate::kit::tui_render_unit::TuiAssistantBubble;
use crate::kit::tui_render_unit::TuiReasoningBlock;
use crate::kit::tui_render_unit::TuiRenderUnit;

/// `BgLiveDetail` 的分层驻留计量。
///
/// `stream` 与 `nested_units` 是两份真实分配（stream 的 bubble 在投影时被
/// clone 到 nested），两者都按实际持有量计入；`tool_cards` 计入累积器自身
/// 持有的字符串、`raw_input` 与 presentation，不假设它们与展示卡共享分配。
/// `body` 是逻辑正文字节（同一正文出现在多个槽位会重复计），`reserved` 才按
/// 分配身份去重；两者口径不同，不得直接相减。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BgLiveMemoryBytes {
    pub summary_bytes: usize,
    pub output_preview_bytes: usize,
    pub subagent_result_bytes: usize,
    pub cancel_reason_bytes: usize,
    pub metadata_bytes: usize,
    pub nested_units_bytes: usize,
    pub stream_bytes: usize,
    pub tool_cards_bytes: usize,
    pub body: usize,
    pub reserved: usize,
}

impl BgLiveDetail {
    /// 只读分层计量；不改变任何状态。
    pub(crate) fn memory_bytes(&self) -> BgLiveMemoryBytes {
        let summary = layer_bytes(&self.summary);
        let output_preview = self
            .output_preview
            .as_ref()
            .map(layer_bytes)
            .unwrap_or_default();
        let subagent_result = self
            .subagent_result
            .as_ref()
            .map(layer_bytes)
            .unwrap_or_default();
        let cancel_reason = self
            .cancel_reason
            .as_ref()
            .map(layer_bytes)
            .unwrap_or_default();
        let mut metadata = (
            self.kind.len(),
            self.kind.capacity() + std::mem::size_of::<Self>(),
        );
        let identity_values = [&self.agent_id, &self.agent_name, &self.subagent_instance_id]
            .into_iter()
            .flatten();
        for value in identity_values {
            metadata.0 += value.len();
            metadata.1 += value.capacity();
        }
        let nested_units = nested_units_layer_bytes(self);
        let stream = self
            .stream
            .as_ref()
            .map(BgStream::memory_bytes)
            .unwrap_or_default();
        let tool_cards = tool_cards_layer_bytes(&self.tool_cards);
        BgLiveMemoryBytes {
            summary_bytes: summary.0,
            output_preview_bytes: output_preview.0,
            subagent_result_bytes: subagent_result.0,
            cancel_reason_bytes: cancel_reason.0,
            metadata_bytes: metadata.0,
            nested_units_bytes: nested_units.0,
            stream_bytes: stream.0,
            tool_cards_bytes: tool_cards.0,
            body: summary.0
                + output_preview.0
                + subagent_result.0
                + cancel_reason.0
                + metadata.0
                + nested_units.0
                + stream.0
                + tool_cards.0,
            reserved: summary.1
                + output_preview.1
                + subagent_result.1
                + cancel_reason.1
                + metadata.1
                + nested_units.1
                + stream.1
                + tool_cards.1,
        }
    }

    /// Disposable display payload only. Active routing identity is O(A), and
    /// copies held by a frame/detail source remain owned by those consumers.
    pub(crate) fn preview_reserved(&self) -> usize {
        let memory = self.memory_bytes();
        let identity = std::mem::size_of::<Self>()
            + self.kind.capacity()
            + [&self.agent_id, &self.agent_name, &self.subagent_instance_id]
                .into_iter()
                .flatten()
                .map(String::capacity)
                .sum::<usize>();
        memory.reserved.saturating_sub(identity)
    }
}

// The owned container reference preserves allocation capacity for retained-memory accounting.
pub(super) fn layer_bytes(value: &String) -> (usize, usize) {
    (value.len(), value.capacity())
}

#[derive(Debug, Clone)]
pub(crate) struct BgStream {
    pub(super) bubble: TuiAssistantBubble,
    pub(super) text_hash: u64,
    pub(super) reasoning_hash: u64,
    pub(super) projected: bool,
    pub(super) dirty: bool,
}

impl BgStream {
    pub(super) fn new(detail: &BgLiveDetail, message_id: Option<String>) -> Self {
        let (bubble, projected) = match detail.nested_units.back() {
            Some(TuiRenderUnit::TuiAssistantBubble(bubble)) => ((**bubble).clone(), true),
            _ => (
                TuiAssistantBubble {
                    text: String::new(),
                    reasoning: None,
                    message_id,
                    started_at: None,
                    duration_ms: None,
                    content_hash: 0,
                },
                false,
            ),
        };
        Self {
            text_hash: crate::kit::tui_render_unit::tui_hash_roll(&bubble.text),
            reasoning_hash: bubble
                .reasoning
                .as_ref()
                .map(|block| crate::kit::tui_render_unit::tui_hash_roll(&block.text))
                .unwrap_or(0),
            bubble,
            projected,
            dirty: false,
        }
    }

    /// stream 的 bubble 是独立分配：投影到 `nested_units` 时使用的是 clone，
    /// 原件仍由 stream 持有，因此 projected 状态也必须计入。
    pub(super) fn memory_bytes(&self) -> (usize, usize) {
        let body = self.bubble.text.len()
            + self
                .bubble
                .reasoning
                .as_ref()
                .map_or(0, |block| block.text.len())
            + self.bubble.message_id.as_ref().map_or(0, String::len);
        let reserved = self.bubble.text.capacity()
            + self
                .bubble
                .reasoning
                .as_ref()
                .map_or(0, |block| block.text.capacity())
            + self.bubble.message_id.as_ref().map_or(0, String::capacity);
        (body, reserved)
    }
}

pub(super) fn nested_units_layer_bytes(detail: &BgLiveDetail) -> (usize, usize) {
    let mut seen = crate::kit::retained_bytes::SeenAllocations::default();
    let reserved = detail
        .nested_units
        .iter()
        .map(|unit| crate::kit::retained_bytes::retained_unit_bytes(unit, &mut seen))
        .sum::<usize>();
    // body 按出现次数累计逻辑正文；reserved 才按共享 Arc 去重。两者口径不同。
    let body = detail.nested_units.iter().map(unit_body_bytes).sum();
    (body, reserved)
}

/// 工具累积器实际持有的字段：字符串、`raw_input`、presentation。
///
/// `nested_units` 中的展示卡是另一份投影（独立分配），这里不做“去重”假设。
// The owned container reference preserves allocation capacity for retained-memory accounting.
pub(super) fn tool_cards_layer_bytes(cards: &Vec<ToolCardAccumulator>) -> (usize, usize) {
    let slot = cards.len() * std::mem::size_of::<ToolCardAccumulator>();
    let mut body = slot;
    let mut reserved = cards.capacity() * std::mem::size_of::<ToolCardAccumulator>();
    for card in cards {
        body += card.tool_id.len()
            + card.tool_name.len()
            + card.input_summary.len()
            + card.output_summary.as_ref().map_or(0, String::len);
        reserved += card.tool_id.capacity()
            + card.tool_name.capacity()
            + card.input_summary.capacity()
            + card.output_summary.as_ref().map_or(0, String::capacity);
        let raw = crate::kit::retained_bytes::json_value_bytes(&card.raw_input);
        body += raw.body;
        reserved += preview::json_reserved(&card.raw_input);
        let presentation = crate::kit::retained_bytes::presentation_bytes(&card.presentation);
        body += presentation.body;
        reserved += preview::presentation_reserved(&card.presentation);
    }
    (body, reserved)
}

pub(super) fn unit_body_bytes(unit: &TuiRenderUnit) -> usize {
    match unit {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            bubble.text.len()
                + bubble
                    .reasoning
                    .as_ref()
                    .map_or(0, |block| block.text.len())
                + bubble.message_id.as_ref().map_or(0, String::len)
        }
        TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.len(),
        TuiRenderUnit::TuiToolCard(card) => {
            card.tool_id.len()
                + card.tool_name.len()
                + card.input_summary.len()
                + card.output_summary.len()
                + crate::kit::retained_bytes::presentation_bytes(&card.presentation).body
                + card.diff.as_ref().map_or(0, |diff| {
                    diff.path.len()
                        + diff
                            .hunks
                            .iter()
                            .map(|hunk| {
                                hunk.old_range.len()
                                    + hunk.new_range.len()
                                    + hunk.lines.iter().map(|line| line.text.len()).sum::<usize>()
                            })
                            .sum::<usize>()
                })
        }
        TuiRenderUnit::TuiSystemNote(note) => note.text.len(),
        TuiRenderUnit::TuiSystemReminder(reminder) => {
            reminder.summary.len()
                + reminder.body.len()
                + reminder.category.len()
                + reminder.source.len()
        }
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            group.instance_id.len()
                + group.agent_id.len()
                + group.agent_name.len()
                + group.error_reason.as_ref().map_or(0, String::len)
                + group.view_models.iter().map(unit_body_bytes).sum::<usize>()
        }
        TuiRenderUnit::TuiDivider(divider) => divider.label.as_ref().map_or(0, String::len),
        TuiRenderUnit::TuiAskUserBlock(block) => {
            block.verb.len()
                + block.question.len()
                + block.result.as_ref().map_or(0, String::len)
                + block.request_id.as_ref().map_or(0, String::len)
                + block.options.iter().map(String::len).sum::<usize>()
                + block
                    .items
                    .iter()
                    .map(|item| item.header.len() + item.answer.len())
                    .sum::<usize>()
                + block.question_ids.iter().map(String::len).sum::<usize>()
        }
        TuiRenderUnit::TuiTodoSummary(summary) => summary.text.len(),
    }
}

pub(super) fn flush_detail(detail: &mut BgLiveDetail) -> bool {
    let Some(stream) = detail.stream.as_mut().filter(|stream| stream.dirty) else {
        return false;
    };
    stream.bubble.content_hash = TuiAssistantBubble::compute_hash_from_rolls(
        stream.text_hash,
        stream.reasoning_hash,
        stream.bubble.reasoning.as_ref(),
        stream.bubble.duration_secs(),
        stream.bubble.started_at.is_none(),
    );
    if stream.projected {
        detail.nested_units.pop_back();
    }
    detail
        .nested_units
        .push_back(TuiRenderUnit::TuiAssistantBubble(
            stream.bubble.clone().into(),
        ));
    stream.projected = true;
    stream.dirty = false;
    true
}

#[must_use]
pub(crate) fn has_pending_streams() -> bool {
    BG_LIVE_DETAIL
        .state()
        .read()
        .values()
        .any(|detail| detail.stream.as_ref().is_some_and(|stream| stream.dirty))
}

pub(crate) fn publish_pending_streams() {
    if !has_pending_streams() {
        return;
    }
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    for detail in map.values_mut() {
        flush_detail(detail);
    }
    preview::enforce(&mut map);
}

pub(super) fn append_stream(
    agent_id: &str,
    message_id: Option<String>,
    chunk: &str,
    reasoning: bool,
) {
    let Some(task_id) = crate::kit::bg_task_identity::task_id_for_agent_id(agent_id) else {
        return;
    };
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write_no_update();
    let Some(detail) = map.get_mut(&task_id) else {
        return;
    };
    if detail.status != BgLiveStatus::Running {
        return;
    }
    if detail.stream.is_none() {
        detail.stream = Some(BgStream::new(detail, message_id));
    }
    let stream = detail
        .stream
        .as_mut()
        .expect("a running detail has a stream initialized before appending");
    if reasoning {
        let block = stream
            .bubble
            .reasoning
            .get_or_insert_with(|| TuiReasoningBlock {
                text: String::new(),
                fold: FoldState::Preview,
                status: EntryStatus::Running,
                is_running: true,
                started_at: Some(peri_time::monotonic_now()),
                duration_ms: None,
            });
        let previous_len = block.text.len();
        preview::append(&mut block.text, chunk, &mut detail.preview_omitted);
        stream.reasoning_hash = if previous_len + chunk.len() == block.text.len() {
            crate::kit::tui_render_unit::tui_hash_roll_update(stream.reasoning_hash, chunk)
        } else {
            crate::kit::tui_render_unit::tui_hash_roll(&block.text)
        };
    } else {
        stream
            .bubble
            .started_at
            .get_or_insert_with(peri_time::monotonic_now);
        let previous_len = stream.bubble.text.len();
        preview::append(&mut stream.bubble.text, chunk, &mut detail.preview_omitted);
        stream.text_hash = if previous_len + chunk.len() == stream.bubble.text.len() {
            crate::kit::tui_render_unit::tui_hash_roll_update(stream.text_hash, chunk)
        } else {
            crate::kit::tui_render_unit::tui_hash_roll(&stream.bubble.text)
        };
    }
    stream.dirty = true;
    preview::touch(detail);
    preview::enforce(&mut map);
}

pub(super) fn with_live_detail<F>(task_id: &str, f: F)
where
    F: FnOnce(&mut BgLiveDetail),
{
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    let detail = map.entry(task_id.to_string()).or_default();
    flush_detail(detail);
    detail.stream = None;
    f(detail);
    preview::touch(detail);
    preview::enforce(&mut map);
}

pub(super) fn with_live_detail_for_agent<F>(agent_id: &str, f: F)
where
    F: FnOnce(&str, &mut BgLiveDetail),
{
    let Some(task_id) = crate::kit::bg_task_identity::task_id_for_agent_id(agent_id) else {
        return;
    };
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    if let Some(detail) = map.get_mut(&task_id) {
        if detail.status != BgLiveStatus::Running {
            return;
        }
        flush_detail(detail);
        detail.stream = None;
        f(&task_id, detail);
        preview::touch(detail);
    }
    preview::enforce(&mut map);
}

pub(super) fn sync_tool_units(detail: &mut BgLiveDetail) {
    if detail.tool_cards.is_empty() {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    let mut units = detail.nested_units.clone();
    for unit in units.iter_mut() {
        let TuiRenderUnit::TuiToolCard(existing) = unit else {
            continue;
        };
        let Some(acc) = detail
            .tool_cards
            .iter()
            .find(|acc| acc.tool_id == existing.tool_id)
        else {
            continue;
        };
        seen.insert(acc.tool_id.clone());
        *unit = TuiRenderUnit::TuiToolCard(detail_tool_card(detail, acc));
    }
    for acc in &detail.tool_cards {
        if seen.insert(acc.tool_id.clone()) {
            units.push_back(TuiRenderUnit::TuiToolCard(detail_tool_card(detail, acc)));
        }
    }
    detail.nested_units = units;
}

pub(super) fn detail_tool_card(
    detail: &BgLiveDetail,
    tool: &ToolCardAccumulator,
) -> crate::kit::tui_render_unit::TuiToolCard {
    let mut card =
        crate::kit::acp_types::build_tool_card(tool, detail.status == BgLiveStatus::Running);
    if detail.status == BgLiveStatus::Unobserved && tool.output_summary.is_none() {
        card.is_error = true;
        card.output_summary = crate::i18n::tr("shell-detail-status-unobserved");
        card.fold = crate::kit::tui_render_unit::fold_for_status(
            crate::kit::tui_render_unit::FoldTarget::Tool,
            EntryStatus::Error,
        );
        card.recompute_hash();
    }
    card
}

pub(super) fn finalize_nested_reasoning(detail: &mut BgLiveDetail) {
    if !detail.nested_units.iter().any(|unit| {
        matches!(unit, TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.reasoning.as_ref().is_some_and(|block| {
            block.is_running || block.started_at.is_some() || block.status != EntryStatus::Completed
        }))
    }) {
        return;
    }
    for unit in detail.nested_units.iter_mut() {
        let TuiRenderUnit::TuiAssistantBubble(bubble) = unit else {
            continue;
        };
        if !bubble.reasoning.as_ref().is_some_and(|block| {
            block.is_running || block.started_at.is_some() || block.status != EntryStatus::Completed
        }) {
            continue;
        }
        let mut bubble = (**bubble).clone();
        if let Some(reasoning) = bubble.reasoning.as_mut() {
            reasoning.duration_ms = reasoning.duration_ms.or_else(|| {
                reasoning
                    .started_at
                    .map(|started| peri_time::elapsed_since(started).as_millis() as u64)
            });
            reasoning.started_at = None;
            reasoning.is_running = false;
            reasoning.status = EntryStatus::Completed;
            reasoning.fold = FoldState::Collapsed;
        }
        bubble.recompute_hash();
        *unit = TuiRenderUnit::TuiAssistantBubble(bubble.into());
    }
}

pub fn init_agent_live_detail(
    task_id: &str,
    agent_id: &str,
    agent_name: &str,
    subagent_instance_id: Option<&str>,
) {
    with_live_detail(task_id, |d| {
        if matches!(
            d.status,
            BgLiveStatus::Succeeded | BgLiveStatus::Failed | BgLiveStatus::Cancelled
        ) {
            return;
        }
        d.agent_id = Some(agent_id.to_string());
        d.agent_name = Some(preview::text(
            agent_name,
            preview::FIELD_BYTES,
            &mut d.preview_omitted,
        ));
        d.subagent_instance_id = subagent_instance_id.map(str::to_string);
        d.status = BgLiveStatus::Running;
    });
}

/// 为「任务行尚未出现」的后台 child 建立占位 live owner（key = agent_id）。
///
/// `SubagentStarted` 与 `BgTaskStarted` 来自不同通道，任务行可能晚到；占位 owner
/// 让迟到的文本/工具/终态先有归属，行到达后由 [`attach_pending_live_detail`] 移交。
pub(crate) fn init_pending_agent_live_detail(
    agent_id: &str,
    agent_name: &str,
    subagent_instance_id: Option<&str>,
) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    // occurrence 变化（resume 复用 agent_id）必须丢弃旧运行正文，即使旧 Stop
    // 丢失后状态仍是 Running；同一 occurrence 的重复 Started 保留终态防复活。
    let occurrence_changed = subagent_instance_id.is_some()
        && map
            .get(agent_id)
            .is_some_and(|detail| detail.subagent_instance_id.as_deref() != subagent_instance_id);
    if occurrence_changed {
        map.remove(agent_id);
    }
    let detail = map.entry(agent_id.to_string()).or_default();
    if matches!(
        detail.status,
        BgLiveStatus::Succeeded | BgLiveStatus::Failed | BgLiveStatus::Cancelled
    ) {
        return;
    }
    detail.agent_id = Some(agent_id.to_string());
    detail.agent_name = Some(preview::text(
        agent_name,
        preview::FIELD_BYTES,
        &mut detail.preview_omitted,
    ));
    detail.subagent_instance_id = subagent_instance_id.map(str::to_string);
    detail.status = BgLiveStatus::Running;
    detail.pending_owner = true;
    preview::touch(detail);
    preview::enforce(&mut map);
}

/// 当前 occurrence 仍在等待任务行的占位 owner（key = agent_id）。
///
/// 只有 Running 占位优先于旧保留行/旧 identity；Stop 后的终态占位不再抢路由。
pub(crate) fn pending_live_owner_for_agent(agent_id: &str) -> Option<String> {
    BG_LIVE_DETAIL
        .state()
        .read()
        .get(agent_id)
        .filter(|detail| detail.pending_owner && detail.status == BgLiveStatus::Running)
        .map(|_| agent_id.to_string())
}

/// 占位 owner 的展示名（绑定任务行时写入 identity）。
pub(crate) fn pending_live_owner_name(agent_id: &str) -> Option<String> {
    BG_LIVE_DETAIL
        .state()
        .read()
        .get(agent_id)
        .filter(|detail| detail.pending_owner)
        .map(|detail| detail.agent_name.clone().unwrap_or_default())
}

/// 把占位 owner 的正文与终态移交给迟到的任务行。
///
/// 任务行已由 `seed_live_from_started` 提供权威 kind/summary/pid；占位 owner 提供
/// child 正文与终态。
pub(crate) fn attach_pending_live_detail(task_id: &str, agent_id: &str) {
    if task_id == agent_id {
        return;
    }
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    let Some(mut pending) = map.remove(agent_id) else {
        return;
    };
    pending.pending_owner = false;
    let Some(detail) = map.get_mut(task_id) else {
        map.insert(task_id.to_string(), pending);
        preview::enforce(&mut map);
        return;
    };
    if !pending.kind.is_empty() {
        detail.kind = std::mem::take(&mut pending.kind);
    }
    if !pending.summary.is_empty() {
        detail.summary = std::mem::take(&mut pending.summary);
    }
    if pending.pid.is_some() {
        detail.pid = pending.pid;
    }
    let agent_id = pending.agent_id.take().or_else(|| detail.agent_id.take());
    detail.agent_id = agent_id;
    let agent_name = pending
        .agent_name
        .take()
        .or_else(|| detail.agent_name.take());
    detail.agent_name = agent_name;
    let instance = pending
        .subagent_instance_id
        .take()
        .or_else(|| detail.subagent_instance_id.take());
    detail.subagent_instance_id = instance;
    detail.nested_units = std::mem::take(&mut pending.nested_units);
    detail.tool_cards = std::mem::take(&mut pending.tool_cards);
    detail.stream = pending.stream.take();
    detail.subagent_result = pending.subagent_result.take();
    detail.subagent_is_error = pending.subagent_is_error;
    detail.preview_omitted |= pending.preview_omitted;
    detail.duration_ms = detail.duration_ms.or(pending.duration_ms);
    detail.output_preview = detail
        .output_preview
        .take()
        .or_else(|| pending.output_preview.take());
    detail.cancel_reason = detail
        .cancel_reason
        .take()
        .or_else(|| pending.cancel_reason.take());
    if pending.status != BgLiveStatus::Running {
        detail.status = pending.status;
        preview::finish(detail);
    }
    preview::touch(detail);
    preview::enforce(&mut map);
}

pub fn seed_live_from_started(task_id: &str, kind: &str, summary: &str, pid: Option<u32>) {
    with_live_detail(task_id, |d| {
        if matches!(
            d.status,
            BgLiveStatus::Succeeded | BgLiveStatus::Failed | BgLiveStatus::Cancelled
        ) {
            return;
        }
        d.kind = kind.to_string();
        d.summary = preview::text(summary, preview::FIELD_BYTES, &mut d.preview_omitted);
        d.pid = pid;
        d.status = BgLiveStatus::Running;
    });
}

/// 当前权威 live 快照显示 running 时建立/恢复 live 证据。
///
/// [Why] 快照会周期性到达，已是 Running 的 detail 不重置 stream——否则正在流式输出的
/// 文本会被切成多段。Unobserved（lost 删除后）或缺失的 detail 则重建为 Running；
/// 既有 tool_cards / nested_units 保留并重新同步，恢复后未完成的工具重新显示为运行中。
pub(crate) fn seed_live_from_running_snapshot(
    task_id: &str,
    kind: &str,
    summary: &str,
    pid: Option<u32>,
) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    let detail = map.entry(task_id.to_string()).or_default();
    if matches!(
        detail.status,
        BgLiveStatus::Succeeded | BgLiveStatus::Failed | BgLiveStatus::Cancelled
    ) {
        return;
    }
    detail.kind = kind.to_string();
    detail.summary = preview::text(summary, preview::FIELD_BYTES, &mut detail.preview_omitted);
    detail.pid = pid;
    if detail.status == BgLiveStatus::Running {
        preview::enforce(&mut map);
        return;
    }
    flush_detail(detail);
    detail.stream = None;
    detail.status = BgLiveStatus::Running;
    sync_tool_units(detail);
    preview::touch(detail);
    preview::enforce(&mut map);
}

pub fn seed_live_from_terminal_snapshot(
    task_id: &str,
    kind: &str,
    summary: &str,
    pid: Option<u32>,
    status: &str,
) {
    let missing = !BG_LIVE_DETAIL.state().read().contains_key(task_id);
    with_live_detail(task_id, |detail| {
        detail.preview_omitted |= missing;
        detail.kind = kind.to_string();
        detail.summary = preview::text(summary, preview::FIELD_BYTES, &mut detail.preview_omitted);
        detail.pid = pid;
        detail.status = match status {
            "completed" => BgLiveStatus::Succeeded,
            "failed" => BgLiveStatus::Failed,
            "cancelled" => BgLiveStatus::Cancelled,
            _ => return,
        };
        preview::finish(detail);
    });
}

pub(crate) fn handle_bg_tool_started(
    agent_id: &str,
    ts: &TuiToolStarted,
    previous_todos: Option<&crate::kit::tool_semantics::TodoSnapshot>,
    todo_baseline_unknown: bool,
) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        let raw_input = preview::raw_input(&ts.raw_input, &mut detail.preview_omitted);
        let input_omitted = !ts.raw_input.is_null() && raw_input.is_null();
        let previous_fits = previous_todos.is_none_or(|todos| {
            std::mem::size_of_val(todos.items())
                + todos
                    .items()
                    .iter()
                    .map(|item| {
                        item.content.capacity()
                            + item.active_form.as_ref().map_or(0, String::capacity)
                    })
                    .sum::<usize>()
                <= preview::FIELD_BYTES
        });
        let mut tool = ToolCardAccumulator::with_input(
            ts.tool_id.clone(),
            preview::text(
                &ts.tool_name,
                preview::FIELD_BYTES,
                &mut detail.preview_omitted,
            ),
            preview::text(
                &ts.input_summary,
                preview::FIELD_BYTES,
                &mut detail.preview_omitted,
            ),
            raw_input,
            previous_todos.filter(|_| previous_fits),
        );
        if !previous_fits || input_omitted {
            detail.preview_omitted = true;
            tool.presentation = crate::kit::tui_render_unit::TuiToolPresentation::Generic;
        }
        if ts.tool_name == "TodoWrite" && todo_baseline_unknown {
            tool.presentation = crate::kit::tui_render_unit::TuiToolPresentation::Generic;
        }
        if let Some(existing) = detail
            .tool_cards
            .iter_mut()
            .find(|existing| existing.tool_id == ts.tool_id)
        {
            if existing.upgrade_input(tool) {
                sync_tool_units(detail);
            }
            return;
        }
        detail.tool_cards.push(tool);
        sync_tool_units(detail);
    });
}

pub fn handle_bg_tool_ended(agent_id: &str, te: &TuiToolEnded) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        let Some(t) = detail
            .tool_cards
            .iter_mut()
            .find(|t| t.tool_id == te.tool_id)
        else {
            return;
        };
        let output = preview::text(
            &te.output_summary,
            preview::FIELD_BYTES,
            &mut detail.preview_omitted,
        );
        if t.finish(output, te.is_error) {
            sync_tool_units(detail);
        }
    });
}

pub fn append_bg_text_chunk(agent_id: &str, tc: &TuiTextChunk) {
    if tc.text.is_empty() {
        return;
    }
    append_stream(agent_id, tc.message_id.clone(), &tc.text, false);
}

pub fn append_bg_reasoning_chunk(agent_id: &str, rc: &TuiReasoningChunk) {
    if rc.text.is_empty() {
        return;
    }
    append_stream(agent_id, rc.message_id.clone(), &rc.text, true);
}

pub(crate) fn append_bg_system_reminder(
    agent_id: &str,
    reminder: crate::kit::tui_render_unit::TuiSystemReminder,
    omitted: bool,
) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        detail.preview_omitted |= omitted;
        detail
            .nested_units
            .push_back(TuiRenderUnit::TuiSystemReminder(reminder));
    });
}

pub fn handle_bg_subagent_stopped(agent_id: &str, result: &str, is_error: bool) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        detail.subagent_result = Some(preview::text(
            result,
            preview::FIELD_BYTES,
            &mut detail.preview_omitted,
        ));
        detail.subagent_is_error = is_error;
        detail.status = if is_error {
            BgLiveStatus::Failed
        } else {
            BgLiveStatus::Succeeded
        };
        preview::finish(detail);
    });
}

pub fn mark_task_completed(
    task_id: &str,
    success: bool,
    duration_ms: u64,
    output_preview: Option<String>,
) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    // A completion for an evicted detail does not recreate its preview.
    if let Some(d) = map.get_mut(task_id) {
        flush_detail(d);
        d.stream = None;
        d.duration_ms = Some(duration_ms);
        d.output_preview = output_preview
            .filter(|s| !s.is_empty())
            .map(|s| preview::text(&s, preview::FIELD_BYTES, &mut d.preview_omitted));
        d.status = if success {
            BgLiveStatus::Succeeded
        } else {
            BgLiveStatus::Failed
        };
        preview::finish(d);
        preview::touch(d);
    }
    preview::enforce(&mut map);
}

pub fn mark_task_cancelled(task_id: &str, reason: &str) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    if let Some(d) = map.get_mut(task_id) {
        flush_detail(d);
        d.stream = None;
        d.cancel_reason = Some(preview::text(
            reason,
            preview::FIELD_BYTES,
            &mut d.preview_omitted,
        ));
        d.status = BgLiveStatus::Cancelled;
        preview::finish(d);
        preview::touch(d);
    }
    preview::enforce(&mut map);
}

pub fn seed_unobserved_snapshot(task_id: &str, kind: &str, summary: &str, pid: Option<u32>) {
    with_live_detail(task_id, |detail| {
        detail.kind = kind.to_string();
        detail.summary = preview::text(summary, preview::FIELD_BYTES, &mut detail.preview_omitted);
        detail.pid = pid;
        detail.status = BgLiveStatus::Unobserved;
        finalize_nested_reasoning(detail);
        sync_tool_units(detail);
    });
}

/// Missing from a current full task snapshot is loss of live evidence, not a
/// task cancellation. Its disposable detail joins the bounded unobserved cache.
pub(crate) fn reconcile_snapshot_tasks(observed: &std::collections::HashSet<String>) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    for (id, detail) in map.iter_mut() {
        // 占位 owner 以 agent_id 为键，本来就不在任务快照中；不能当成 lost 证据。
        if detail.status == BgLiveStatus::Running && !detail.pending_owner && !observed.contains(id)
        {
            flush_detail(detail);
            detail.stream = None;
            detail.status = BgLiveStatus::Unobserved;
            detail.preview_omitted = true;
            finalize_nested_reasoning(detail);
            sync_tool_units(detail);
            preview::touch(detail);
        }
    }
    preview::enforce(&mut map);
}

use crate::kit::atoms::BG_LIVE_DETAIL;

use crate::kit::atoms::BgLiveDetail;

use crate::kit::atoms::BgLiveStatus;

use crate::kit::stream_data::TuiReasoningChunk;

use crate::kit::stream_data::TuiTextChunk;

use crate::kit::stream_data::TuiToolEnded;

use crate::kit::stream_data::TuiToolStarted;
