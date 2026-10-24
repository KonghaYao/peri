//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

// ── Sub-modules ──
mod agent;
mod compact;
mod fold;
mod history_page;
pub(crate) mod history_window;
pub(crate) mod live_tool_links;
mod memory_probe;
pub(crate) mod render;
mod streaming;
mod subagent;
mod system;
mod tool;
mod turn;

// ── Re-exports for external consumers ──
pub use self::render::handle_plan_update;
pub(crate) use self::render::push_view_models;
pub use self::render::push_view_models_for_reset;
pub(crate) use self::system::request_bg_task_snapshot;
// 测试文件通过 super::* 获取这些类型——原在 acp_events.rs 中直接可用
#[cfg(test)]
pub(crate) use self::render::drain_input_buffer;
pub use crate::kit::submit_request::SubmitRequest;
pub use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit, TuiUserBubble};

use crate::kit::acp_types::{AcpEventData, CacheUsageSample, CurrentTurn};
use crate::kit::atoms::*;
use crate::kit::tui_render_unit::{TuiNoteLevel, UserAttachment, tui_hash_str};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// StreamingMode — 流式渲染模式控制
// ---------------------------------------------------------------------------

/// streaming_mode 配置映射。命名与 settings.json 中的值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamingMode {
    /// 逐 token 推送（默认行为）
    Streaming,
    /// 按 Markdown 块边界推送
    Block,
    /// 不推送中间内容，仅 TurnDone 时推送
    None,
}

/// 从 TUI_CONFIG_HANDLE 即地读取当前 streaming_mode。
/// 每次流式事件进入时调用——配置热切换即时生效。
pub(crate) fn current_streaming_mode() -> StreamingMode {
    let handle = match crate::kit::atoms::TUI_CONFIG_HANDLE.get() {
        Some(h) => h,
        None => return StreamingMode::Streaming,
    };
    let guard = match handle.try_read() {
        Some(g) => g,
        None => return StreamingMode::Streaming,
    };
    match guard.streaming_mode.as_deref() {
        Some("block") => StreamingMode::Block,
        Some("none") => StreamingMode::None,
        _ => StreamingMode::Streaming,
    }
}

/// 检测未发布文本中的新增 Markdown 块边界，偏移均为字节数。
///
/// 块边界定义：
/// - 两个连续换行（段落分隔）
/// - 以 `#` 开头的行（标题）
/// - 以 ``` 开头的行（代码块开始/结束）
/// - 以 `---`、`***`、`___` 开头的行（水平线）
///
/// 首次推送直接返回；其余只扫描新 chunk，回看最多两个字节识别跨 chunk 标记。
/// 新 chunk 含换行时才检查未发布前缀；两个换行即发布，因此前缀扫描摊销线性。
fn has_md_block_boundary_since(full_text: &str, published: usize, chunk_start: usize) -> bool {
    if published == 0 {
        return true;
    }
    let bytes = full_text.as_bytes();
    let new_start = chunk_start.max(published).min(bytes.len());
    if new_start == bytes.len() {
        return false;
    }
    let scan_start = new_start.saturating_sub(2).max(published);
    for offset in scan_start..bytes.len() {
        if offset == 0 || bytes[offset - 1] == b'\n' {
            let byte = bytes[offset];
            if (byte == b'#' && bytes.get(offset + 1).is_none_or(|next| *next == b' '))
                || (matches!(byte, b'`' | b'-' | b'*' | b'_')
                    && bytes.get(offset + 1) == Some(&byte)
                    && bytes.get(offset + 2) == Some(&byte))
            {
                return true;
            }
        }
    }
    let new_lines = bytes[new_start..]
        .iter()
        .filter(|byte| **byte == b'\n')
        .take(2)
        .count();
    new_lines >= 2 || (new_lines == 1 && bytes[published..new_start].contains(&b'\n'))
}

// ---------------------------------------------------------------------------
// BridgeState — ACP 事件桥接内部状态
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    PromptRunning,
    ReplayingHistory,
}

/// Composer rollback text is owned by its submitted request, not the active execution.
pub struct SubmittedInputRollback {
    pub text: String,
    pub request_id: Option<String>,
}

/// 桥接任务维护的内部状态，每个 ACP 事件到达时同步更新。
///
/// 定义在 acp_events.rs 中以避免 acp_bridge ↔ acp_events 循环依赖。
pub struct BridgeState {
    /// 0=Idle, 1=Streaming, 2=Modal
    pub variant: u8,
    /// 已提交的 TuiRenderUnit 列表——im::Vector 支持 O(1) clone + O(log n) push_back。
    pub committed: im::Vector<TuiRenderUnit>,
    /// S5：历史窗口模型与账目。默认关闭；关闭时不改变旧渲染/滚动/事件行为。
    /// `committed` 仍是展示正文的唯一强持有者，窗口只保存持久身份与字节账目。
    pub window: crate::kit::history_window::HistoryWindow,
    /// 当前轮次的增量数据
    pub current_turn: CurrentTurn,
    /// 当前 session lifecycle 阶段。loading 只由该阶段派生。
    pub phase: SessionPhase,
    /// S7：精确弹窗类型，由 AcpEvent 直接映射。None = 无弹窗。
    /// 弹窗激活状态由 POPUP_KIND.is_some() 派生（status_bar / event_handlers 都读这个）
    pub popup_kind: Option<PopupKind>,
    /// ViewModelsSnapshot generation——每次 push_view_models 递增。
    /// render_bridge 用此值检测变化（替代原先的 Arc::as_ptr 比较）。
    pub generation: u64,
    /// 当前活跃 session 的 ID。事件携带的 active_session_id 不匹配时丢弃。
    pub active_session_id: String,
    /// `/compact` 刚刚完成、且本轮尚无后续流事件。REQ-C02 / A12：TurnDone
    /// **不再**触发 session/load 重放；标志只用于两件事——清掉仅服务同会话
    /// reset 边界的 `PENDING_COMPACT_NOTE`，以及去重紧随 compact 的 UiOnly 反馈。
    /// S4.1 方案 B：CompactCompleted 置位后，任何流事件（TextChunk/
    /// ReasoningChunk/ToolStarted/ToolEnded/SubagentStarted）到达即清除——
    /// 命令 compact 后无流事件，标志保持；agent 内部 auto-compact 后标志被
    /// 后续流事件清掉（无需知道 compact 触发来源）。
    pub compact_just_completed: bool,
    /// 本地用户提交的文本及身份——仅所属请求的零产出取消可恢复输入框。
    /// LocalUserBubble 到达时写入，TurnInterrupted 零产出时消费并清空。
    pub last_submitted_text: Option<SubmittedInputRollback>,
    /// streaming_mode=block 时追踪上次推送后主 agent 文本的字节数。
    /// 用于 `has_md_block_boundary_since` 的比较基点。
    pub last_pushed_text_len: usize,
    /// streaming_mode=block 时追踪上次推送后主 agent 推理的字节数。
    pub last_pushed_reasoning_len: usize,
    /// 当前会话最近一次成功 TodoWrite 的完整快照；仅用于下一张 Todo 卡片的变更集。
    pub(crate) last_successful_todos: Option<crate::kit::tool_semantics::TodoSnapshot>,
    /// 当前基线对应 TodoWrite 的启动序号，用于拒绝较旧调用的乱序成功结束事件。
    pub(crate) last_successful_todo_sequence: Option<u64>,
    /// 单调递增的 TodoWrite 启动序号。
    pub(crate) next_todo_sequence: u64,
    /// 所有未结束 TodoWrite 的启动序号与原始输入，涵盖主 agent、子 agent 和回放。
    pub(crate) todo_call_inputs: HashMap<String, (u64, serde_json::Value)>,
    /// turn 代际计数器——每次用户可见提交（LocalUserBubble）递增。
    /// 用于识别 stale 的 turn 结束事件（Issue 2026-08-05）：用户取消旧 turn 后
    /// 立即提交新输入时，新输入事件可能先于旧 turn 的 TurnInterrupted 到达，
    /// 该事件会误删新气泡/误恢复旧文本/清空排队输入。代际防护据此跳过回滚。
    pub turn_generation: u64,
    /// 最后一次已真正发出 prompt RPC（PromptSubmitted）时的代际快照。
    /// `turn_generation > last_prompt_generation` 表示存在"已显示气泡但未发出
    /// 请求"的更新提交——此时到达的 TurnInterrupted 属于旧 turn（stale）。
    pub last_prompt_generation: u64,
    /// 当前 turn 的 prompt requestId（PromptSubmitted 时记录，submit_consumer
    /// 生成、服务器经 turn 结束事件回带）。TurnInterrupted 到达时若携带
    /// request_id 且与当前值不匹配 → stale（Issue 2026-08-05 返工：request_id
    /// 配对判定，与 turn_generation 代际判定 OR 组合，覆盖"新提交已发 RPC"的
    /// 主导排序场景；排队分支无 RPC → current_request_id 停留在旧 turn，由
    /// 代际判定兜底）。
    pub current_request_id: Option<String>,
    /// Latest root-agent cache usage observation for this prompt. A root clear
    /// observation assigns `None`, preventing a stale earlier sample from
    /// surviving to `TurnDone`.
    pub pending_cache_usage: Option<CacheUsageSample>,
    pub(crate) publication_intent: PublicationIntent,
    pub(crate) folded_history: fold::FoldedHistory,
}

/// 一次归档中共享同一持久条目身份的 live 单元组。
struct LiveSealGroup {
    key: crate::kit::history_window::PersistentEntryKey,
    /// open 页给出的持久位置；纯 live 归属时为 `None`，等提交/页回执提升。
    position: Option<u64>,
    slots: Vec<usize>,
    body_bytes: usize,
    reserved_bytes: usize,
    /// 组内工具卡的 `(tool_id, 既有 retained 字节)`，用于归属后回落未归属账目。
    tool_bytes: Vec<(String, usize)>,
}

impl BridgeState {
    /// 显式同步 barrier：保留 terminal 发布先于 drain/replay 等副作用的顺序。
    fn publish_barrier(&mut self) {
        render::push_view_models(self);
        self.publication_intent = PublicationIntent::Published;
        self.sample_memory("bridge_publish");
    }

    /// 归档当前轮次中可完成的展示结果。
    ///
    /// running subagent 存在时只搬走第一个仍会增长的实体之前的稳定前缀。已完成但位于
    /// 运行中分组之后的子 Agent **不能**单独抽到 committed：committed 整段渲染在
    /// current_turn 之前，抽走会让它跨越仍在运行的分组，破坏原时间线顺序（R05）。
    fn archive_current_turn(&mut self) {
        let start = self.committed.len();
        // 任何运行中的分组（含后台）都可能继续增长，只能搬稳定前缀；调用方
        // 在此路径不得 reset current_turn。后台组跨 turn 由全局 live 明细维护。
        if self.current_turn.has_running_subagent() {
            // 运行中 subagent 不应让整轮常驻：只把第一个仍会增长的实体之前的稳定
            // 前缀按 segment 顺序交给 committed——文本、已完成工具，以及已停止且已
            // 与父卡片配对的子 Agent。前缀之后的内容仍保留在容器里，等运行中的分组
            // 结束后按原顺序整体归档。
            for vm in self.current_turn.take_archivable_prefix() {
                self.committed.push_back(vm);
            }
        } else {
            self.archive_current_turn_fully();
            return;
        }
        let range = start..self.committed.len();
        self.register_live_seal_segments(range);
    }

    /// 冻结整轮：所有展示单元搬入 committed，活动容器由调用方 reset。
    ///
    /// 用于 turn 边界（stale / cancel / suspend）没有同步运行组的情况；此时
    /// 后台组也必须整体归档，否则调用方的 reset 会把它丢掉。
    fn archive_current_turn_fully(&mut self) {
        let start = self.committed.len();
        // 整轮归档沿用 segment 顺序，不能先抽走子 Agent 再追加根内容。
        if !self.current_turn.committed && !self.current_turn.is_empty() {
            self.current_turn.deactivate();
            for vm in self.current_turn.view_models() {
                self.committed.push_back(vm.clone());
            }
        }
        self.current_turn.take_completed_subagents();
        let range = start..self.committed.len();
        self.register_live_seal_segments(range);
    }

    /// 已产出轮次归档的统一入口：先搬 VM，再按实际槽位登记窗口预算。
    ///
    /// stale / cancel / suspend 等直接归档路径必须共用，避免绕过
    /// `register_live_seal_segments` 形成未记账的驻留正文。
    ///
    /// 返回是否仍有运行中的**同步** subagent。调用方在 true 时不得重置
    /// `current_turn`：同步运行组仍在累积正文，重置会让后续 chunk /
    /// SubagentStopped 丢失，UI 永久停在 running 且缺最终输出。
    /// 后台组跨 turn 由全局 live 明细维护，这里必须按可冻结处理。
    fn commit_current_turn_units(&mut self) -> bool {
        let has_running_sync_subagent = self.current_turn.has_running_sync_subagent();
        if self.current_turn.committed || self.current_turn.is_empty() {
            return has_running_sync_subagent;
        }
        // 同步运行组只归档稳定前缀并保留容器；无同步运行组时整轮冻结，
        // 后台组也必须随之归档，调用方才能安全 reset。
        if has_running_sync_subagent {
            self.archive_current_turn();
        } else {
            self.archive_current_turn_fully();
        }
        has_running_sync_subagent
    }

    /// live 展示单元登记为可淘汰的 LiveSeal 段。
    ///
    /// 工具卡没有 ACP live `messageId`：先按 [`live_tool_links`] 保存的 live 归属
    /// 找所属 assistant 条目；open 页按 `tool_id` 回填的持久条目同样可用。找不到
    /// 归属的单元不伪造身份，继续计入未归属预算。
    ///
    /// 同一消息可能先归档文本、后归档工具卡：身份已驻留在 LiveSeal 段时把新增
    /// 单元并入原段，不登记第二个条目身份（驻留索引要求条目身份唯一）。
    fn register_live_seal_segments(&mut self, slots: impl Iterator<Item = usize>) {
        if !self.window.is_enabled() {
            return;
        }
        use crate::kit::history_window::{
            HistoryEntryView, PersistentEntryKey, SegmentDraft, SegmentOrigin, SegmentPlacement,
            WindowUnitMapping, publish_window_unit_mappings,
        };

        let window_id = self.window.instance_id();
        let generation = self.window.generation();
        let mut groups: Vec<LiveSealGroup> = Vec::new();
        let mut group_indices: HashMap<PersistentEntryKey, usize> = HashMap::new();
        let mut unattributed_bytes = 0usize;
        let mut unattributed_tool_delta = 0i64;
        let mut unattributed_seen = crate::kit::retained_bytes::SeenAllocations::default();
        for slot in slots {
            let unit = &self.committed[slot];
            let unit_bytes =
                crate::kit::retained_bytes::retained_unit_bytes(unit, &mut unattributed_seen);
            let Some((key, position)) = self.live_unit_owner(unit) else {
                // 子 Agent 组、无持久身份的正文：不能建可淘汰段，但正文确实驻留，
                // 必须计入同一预算，避免账目低估。
                if let TuiRenderUnit::TuiToolCard(card) = unit {
                    unattributed_tool_delta += live_tool_links::note_unattributed(
                        &self.active_session_id,
                        window_id,
                        generation,
                        &card.tool_id,
                        unit_bytes,
                    );
                } else {
                    unattributed_bytes += unit_bytes;
                }
                continue;
            };
            // 工具卡正文沿用 `retained_unit_bytes` 的既有口径（containers +
            // summaries + presentation 之外的既有范围）。归属后这部分从
            // unattributed 移入可淘汰段，总账不缩小。
            let body_bytes = match unit {
                TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.len(),
                TuiRenderUnit::TuiAssistantBubble(bubble) => {
                    bubble.text.len() + bubble.reasoning.as_ref().map_or(0, |r| r.text.len())
                }
                TuiRenderUnit::TuiToolCard(_) => unit_bytes,
                _ => 0,
            };
            let index = *group_indices.entry(key.clone()).or_insert_with(|| {
                groups.push(LiveSealGroup {
                    key,
                    position,
                    slots: Vec::new(),
                    body_bytes: 0,
                    reserved_bytes: 0,
                    tool_bytes: Vec::new(),
                });
                groups.len() - 1
            });
            let group = &mut groups[index];
            if position.is_some() {
                group.position = position;
            }
            group.slots.push(slot);
            group.body_bytes += body_bytes;
            group.reserved_bytes += unit_bytes;
            if let TuiRenderUnit::TuiToolCard(card) = unit {
                group.tool_bytes.push((card.tool_id.clone(), unit_bytes));
            }
        }
        if unattributed_bytes > 0 {
            self.window.note_unattributed_live_bytes(unattributed_bytes);
        }
        if unattributed_tool_delta > 0 {
            self.window
                .note_unattributed_live_bytes(unattributed_tool_delta as usize);
        } else if unattributed_tool_delta < 0 {
            self.window
                .adjust_unattributed_live_bytes(unattributed_tool_delta);
        }

        // 回填槽位可能早于既有展示区间：按 `committed` 顺序处理，段的布局位置由
        // 第一个更晚的展示区间决定，不能一律追加到尾部（否则 index 槽位顺序与
        // `committed` 不一致，释放会删错区间）。
        groups.sort_by_key(|group| group.slots.first().copied().unwrap_or(usize::MAX));
        let mut mappings = Vec::new();
        for group in groups {
            // 展示账目冲突意味着回填证据不可信：宁可不建段（继续按未归属记账），
            // 也不能让段的单位数与真实槽位错位。必须在并入既有段之前判定。
            if !group
                .slots
                .iter()
                .all(|slot| self.window.display_slot_available(*slot))
            {
                tracing::warn!(
                    key = ?group.key,
                    slots = ?group.slots,
                    "history_window: live seal display slots already covered"
                );
                self.note_group_unattributed(&group, window_id, generation);
                continue;
            }
            let resident = self.window.index().contains_entry(&group.key);
            let attached = resident
                && self.window.extend_live_entry(
                    &group.key,
                    group.slots.len(),
                    group.body_bytes,
                    group.reserved_bytes,
                );
            if resident && !attached {
                // 同一身份已驻留在页段（非 LiveSeal）：不能并入，正文仍是真实占用。
                self.note_group_unattributed(&group, window_id, generation);
                continue;
            }
            if !resident {
                let entry = HistoryEntryView::new(
                    group.key.clone(),
                    group.position,
                    group.slots.len(),
                    group.body_bytes,
                    group.reserved_bytes,
                    Vec::new(),
                );
                let draft = SegmentDraft::new(
                    group.key.source.clone(),
                    vec![entry],
                    group.body_bytes,
                    group.reserved_bytes,
                )
                .durable(false)
                .origin(SegmentOrigin::LiveSeal);
                let placement = self
                    .window
                    .segment_after_display_slot(group.slots[0])
                    .map_or(SegmentPlacement::Tail, SegmentPlacement::BeforeSegment);
                if let Err(error) = self.window.register_segment(draft, placement) {
                    tracing::error!(?error, "history_window: live seal registration failed");
                    self.note_group_unattributed(&group, window_id, generation);
                    continue;
                }
            }
            // 归属成功：把先前计入 unattributed 的工具卡字节移出，避免只增不减。
            let mut returned = 0usize;
            for (tool_id, _) in &group.tool_bytes {
                returned += live_tool_links::take_unattributed(
                    &self.active_session_id,
                    window_id,
                    generation,
                    tool_id,
                );
            }
            if returned > 0 {
                self.window.release_unattributed_live_bytes(returned);
            }
            // 单元已在 `committed` 中：只补展示账目，不得右移其他区间。
            for slot in &group.slots {
                if !self
                    .window
                    .mark_existing_display_slot(group.key.clone(), *slot, 1)
                {
                    tracing::error!(
                        slot,
                        key = ?group.key,
                        "history_window: live seal display slot registration lost race"
                    );
                    continue;
                }
                mappings.push(WindowUnitMapping {
                    token: crate::kit::message_area::window_identity_token(&self.committed[*slot]),
                    entry: group.key.clone(),
                });
            }
        }
        publish_window_unit_mappings(mappings);
    }

    /// live 单元到持久条目的定位。
    fn live_unit_owner(
        &self,
        unit: &TuiRenderUnit,
    ) -> Option<(crate::kit::history_window::PersistentEntryKey, Option<u64>)> {
        use crate::kit::history_window::PersistentEntryKey;
        match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => bubble.message_id.as_ref().map(|id| {
                (
                    PersistentEntryKey::own(format!("{}:{id}", self.active_session_id)),
                    None,
                )
            }),
            TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.message_id.as_ref().map(|id| {
                (
                    PersistentEntryKey::own(format!("{}:{id}", self.active_session_id)),
                    None,
                )
            }),
            TuiRenderUnit::TuiToolCard(card) => {
                live_tool_links::owner_for_tool(&self.active_session_id, &card.tool_id)
                    .map(|owner| (owner.entry, owner.position))
            }
            _ => None,
        }
    }

    /// 无法归属的一组单元按未归属正文计入预算。
    fn note_group_unattributed(&mut self, group: &LiveSealGroup, window_id: u64, generation: u64) {
        let mut tool_total = 0usize;
        let mut tool_delta = 0i64;
        for (tool_id, bytes) in &group.tool_bytes {
            tool_total = tool_total.saturating_add(*bytes);
            tool_delta += live_tool_links::note_unattributed(
                &self.active_session_id,
                window_id,
                generation,
                tool_id,
                *bytes,
            );
        }
        let non_tool = group.reserved_bytes.saturating_sub(tool_total);
        if non_tool > 0 {
            self.window.note_unattributed_live_bytes(non_tool);
        }
        if tool_delta > 0 {
            self.window
                .note_unattributed_live_bytes(tool_delta as usize);
        } else if tool_delta < 0 {
            self.window.adjust_unattributed_live_bytes(tool_delta);
        }
    }

    /// 将 current_turn 已产出内容 flush 到 committed，然后 reset。
    ///
    /// 需要保证时序正确性的位置会调用该入口；running subagent 不会清除 current_turn，
    /// 只归档它之前的稳定前缀，其余内容保留到分组结束后按原顺序整体提交。
    fn flush_current_turn(&mut self) {
        // 消息身份只在同一段流式输出内有效；turn 边界后不得沿用到下一轮。
        live_tool_links::note_turn_boundary(&self.active_session_id);
        let has_running_subagent = self.current_turn.has_running_subagent();
        if has_running_subagent {
            tracing::debug!(
                running_count = self
                    .current_turn
                    .subagents
                    .iter()
                    .filter(|subagent| subagent.is_running)
                    .count(),
                "flush_current_turn: running subagent retained while completed content is archived"
            );
        }
        self.archive_current_turn();
        if !has_running_subagent {
            self.current_turn.reset();
        }
        self.sample_memory("bridge_flush");
    }

    /// Put a user prompt after all output already received from the preceding
    /// turn. A live subagent keeps its container in current_turn, so in that
    /// case the prompt joins the same chronological segment stream.
    fn push_user_bubble(
        &mut self,
        text: String,
        attachments: Vec<UserAttachment>,
        message_id: Option<String>,
    ) {
        // A tool result can still arrive after the next prompt is displayed.
        // Preserve its accumulator until ToolEnded; TurnDone retains its normal
        // flush behavior for tools that never emit an end event.
        let has_running_tool = self
            .current_turn
            .tool_cards
            .iter()
            .any(|tool| tool.output_summary.is_none());
        if !has_running_tool {
            self.flush_current_turn();
        }
        let mut bubble = TuiUserBubble::with_attachments(text, attachments);
        bubble.message_id = message_id;
        if self.current_turn.is_empty() {
            let start = self.committed.len();
            self.committed
                .push_back(TuiRenderUnit::TuiUserBubble(bubble));
            self.register_live_seal_segments(start..self.committed.len());
        } else {
            self.current_turn
                .push_user_bubble(bubble.text, bubble.attachments, bubble.message_id);
        }
    }

    /// SystemNote 统一注入入口。封装 push_system_note → push_view_models → push_acp_state
    /// 三步操作，确保 SystemNote 按时序出现在 current_turn 内部。
    ///
    /// 所有 handler（CompactCompleted/CommandFeedback/AgentExecutionFailed/
    /// BudgetWarning/SystemNotification）必须通过此方法注入 SystemNote，
    /// 禁止直接 push committed。
    pub fn inject_system_note(&mut self, text: String, level: TuiNoteLevel) {
        let content_hash = tui_hash_str(&text);
        self.current_turn
            .push_system_note(text, level, content_hash);
        self.publish_barrier();
        render::push_acp_state(self);
    }

    /// 记录 TodoWrite 启动并返回其会话内单调序号。
    pub(crate) fn record_todo_started(&mut self, tool_id: String, raw_input: serde_json::Value) {
        let sequence = self.next_todo_sequence;
        self.next_todo_sequence = self.next_todo_sequence.wrapping_add(1);
        self.todo_call_inputs.insert(tool_id, (sequence, raw_input));
    }

    /// 仅当完成调用不早于当前成功基线时才推进 Todo 快照。
    pub(crate) fn complete_todo_if_current(&mut self, tool_id: &str, is_error: bool) -> bool {
        let Some((sequence, raw_input)) = self.todo_call_inputs.remove(tool_id) else {
            return false;
        };
        if is_error
            || self
                .last_successful_todo_sequence
                .is_some_and(|current| sequence < current)
        {
            return false;
        }
        if let Some(snapshot) = crate::kit::tool_semantics::TodoSnapshot::parse(&raw_input) {
            self.last_successful_todos = Some(snapshot);
            self.last_successful_todo_sequence = Some(sequence);
            return true;
        }
        false
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationIntent {
    #[default]
    None,
    Immediate,
    /// handler 已完成显式同步 barrier，scheduler 只结算 pending。
    Published,
    /// 历史回放等不受 streaming mode 抑制的更新。
    Deferred,
    /// 普通流式更新；deadline 到期时仍须核对模式。
    Streaming,
    /// canonical 已变化，但模式/块边界不允许自动发布。
    Hidden,
}

// ---------------------------------------------------------------------------
// 核心分发函数
// ---------------------------------------------------------------------------

/// 将 AcpEventData 分发到对应的 Atom 写入，并更新 BridgeState。
///
/// 这是 acp_bridge 消费事件时调用的核心函数。
/// 每次调用按事件类型更新内部状态，然后 push 到 VIEW_MODELS 和 ACP_STATE Atoms。
pub(crate) fn dispatch_for_bridge(
    state: &mut BridgeState,
    event: &AcpEventData,
) -> PublicationIntent {
    state.publication_intent = PublicationIntent::None;
    // 回放条目区间登记需要「本事件推入了哪个槽位」——在 handler 之前取长度。
    let committed_len_before = state.committed.len();
    use AcpEventData::*;
    // S4.1 方案 B：CompactCompleted 置 compact_just_completed 后，任何流事件
    // 到达即清除标志——agent 内部 auto-compact 后 ReAct 循环继续产出，流事件
    // （TextChunk/ReasoningChunk/ToolStarted/ToolEnded/SubagentStarted/
    // ToolCount/Progress）必然紧随；命令 /compact（Immediate）后无流事件，
    // 标志保持到 TurnDone——届时只清标志与 `PENDING_COMPACT_NOTE`，不触发重放
    // （REQ-C02 / A12）。无需知道 compact 触发来源（Issue 2026-08-05）。清单与
    // issue 声明清单保持同步（含 ToolCount/Progress）。
    if matches!(
        event,
        TextChunk(_)
            | ReasoningChunk(_)
            | ToolStarted(_)
            | ToolEnded(_)
            | SubagentStarted { .. }
            | ToolCount(_)
            | Progress(_)
    ) {
        if state.compact_just_completed {
            crate::kit::atoms::PENDING_COMPACT_NOTE.set(None);
        }
        state.compact_just_completed = false;
    }
    match event {
        // ── §4.1 Streaming events ──
        TextChunk(tc) => return streaming::handle_text_chunk(state, tc),
        ReasoningChunk(rc) => return streaming::handle_reasoning_chunk(state, rc),

        // ── §4.1 Tools ──
        ToolStarted(ts) => tool::handle_tool_started(state, ts),
        ToolEnded(te) => tool::handle_tool_ended(state, te),

        // ── §4.2 Boundary events ──
        PromptStarted => turn::handle_prompt_started(state),
        PromptSubmitted { request_id } => turn::handle_prompt_submitted(state, request_id),
        ExecutionStarted { request_id } => {
            turn::handle_execution_started(state, &Some(request_id.clone()))
        }
        CacheUsageUpdated(sample) => turn::handle_cache_usage_updated(state, sample),
        SessionReplayStarted => turn::handle_session_replay_started(state),
        SessionReplayDone => {
            // 回放结束：关闭正在铺的条目区间（首屏区间索引在此定型）。
            crate::kit::history_paging::close_own_entry();
            turn::handle_session_replay_done(state)
        }
        TurnDone => turn::handle_turn_done(state),
        AgentDone { request_id } => turn::handle_agent_done(state, request_id),
        TurnInterrupted { reason, request_id } => {
            turn::handle_turn_interrupted(state, reason, request_id)
        }
        TurnSuspended => turn::handle_turn_suspended(state),

        // ── §4.3 Status events ──
        ToolCount(_tc) => tool::handle_tool_count(state),
        Progress(_) => tool::handle_progress(state),
        BudgetWarning(bw) => system::handle_budget_warning(state, bw),
        SystemNotification(sn) => system::handle_system_notification(state, sn),
        SystemReminder {
            reminder,
            source_agent_id,
            ..
        } => system::handle_system_reminder(state, reminder, source_agent_id.as_deref()),
        SystemReminderFallback { text, .. } => system::handle_system_reminder_fallback(state, text),
        CommandFeedback(fb) => system::handle_command_feedback(state, fb),

        // ── §4.4 Input assist ──
        GoalSnapshot {
            objective,
            status,
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason,
        } => system::handle_goal_snapshot(
            objective,
            *status,
            *token_budget,
            *tokens_used,
            *time_used_seconds,
            *continuation_count,
            blocked_reason,
        ),
        Prediction(p) => system::handle_prediction(p),
        FileSuggestions(_) => system::handle_file_suggestions(),

        // ── §4.5 Interaction events ──
        HitlPending(hp) => system::handle_hitl_pending(state, hp),
        AskUser(au) => system::handle_ask_user(state, au),
        // [Slice 4 §6.8] TUI 内部事件：interaction block 结果回写（本地，
        // 不走 ACP 协议）。
        InteractionTerminal { owner, outcome } => {
            system::handle_interaction_terminal(state, owner, outcome)
        }
        // Rewind v2：rewind-preview 推送已退役（候选改由打开面板时实时查询），
        // 变体保留以向后兼容旧服务端。
        RewindPreview(_) => {}
        RewindCompleted { messages_json } => system::handle_rewind_completed(state, messages_json),
        OauthNeeded(on) => system::handle_oauth_needed(state, on),
        OauthCompleted { server_name } => system::handle_oauth_completed(state, server_name),
        OauthFailed { server_name, error } => {
            system::handle_oauth_failed(state, server_name, error)
        }
        OauthRestored { server_name } => system::handle_oauth_restored(state, server_name),

        // ── §4.6 Structure events ──
        SubagentStarted {
            agent_id,
            agent_name,
            is_background,
            parent_tool_call_id,
        } => subagent::handle_subagent_started(
            state,
            agent_id,
            agent_name,
            *is_background,
            parent_tool_call_id.clone(),
        ),
        SubagentStopped {
            agent_id,
            result,
            is_error,
        } => subagent::handle_subagent_stopped(state, agent_id, result, *is_error),

        // ── §4.8 Agent Event Extensions ──
        TurnCommitted {
            messages_json: _,
            steps,
        } => turn::handle_turn_committed(state, *steps),
        CompactStarted => compact::handle_compact_started(state),
        CompactCompleted {
            summary,
            trigger,
            strategy,
            affected_count,
            estimated_tokens_saved,
            files,
            skills,
            ..
        } => compact::handle_compact_completed(
            state,
            summary,
            trigger,
            strategy,
            *affected_count,
            *estimated_tokens_saved,
            files.len(),
            skills.len(),
        ),
        BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            duration_ms,
            ..
        } => agent::handle_background_task_completed(agent_name, task_id, *success, *duration_ms),
        LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => system::handle_llm_retrying(state, *attempt, *max_attempts, *delay_ms, error),
        AgentExecutionFailed { message } => agent::handle_agent_execution_failed(state, message),
        WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            phase,
            ..
        } => system::handle_workflow_progress(run_id, workflow_name, event_type, phase),

        // ── §4.9 Plugin events ──
        PluginSnapshot(snapshot) => system::handle_plugin_snapshot(snapshot),
        PluginActionResult(result) => system::handle_plugin_action_result(result),
        // Legacy notifications lack request identity; Discover owns RPC results.
        PluginSearchResult(_) => {}

        // ── Unknown / forward-compat ──
        Unknown { .. } => system::handle_unknown(),
        LocalUserBubble { text, attachments } => {
            turn::handle_local_user_bubble(state, text, attachments.clone(), None)
        }
        UserInputQueueChanged { snapshot } => {
            crate::kit::steer_state::STEERS
                .state()
                .write()
                .accept_snapshot(
                    snapshot.clone(),
                    crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
                    false,
                );
        }
        UserInputDelivered {
            generation,
            input_id,
            content,
        } => turn::handle_user_input_delivered(state, generation, input_id, content),
        ReplayedUserBubble { input_id, text } => {
            crate::kit::steer_state::STEERS
                .state()
                .write()
                .claim_delivery(&state.active_session_id, input_id);
            turn::handle_local_user_bubble(state, text, Vec::new(), Some(input_id.clone()));
        }
        LocalLoadingReset => turn::handle_loading_reset(state),
        PromptFailed { request_id } => turn::handle_prompt_failed(state, request_id),
        BgCallbackBubble { .. } => turn::handle_bg_callback_bubble(state),
        CommittedAssistantText { text, reasoning } => {
            turn::handle_committed_assistant_text(state, text, reasoning)
        }
        ReplayToolStarted {
            tool_id,
            tool_name,
            input_summary,
            raw_input,
        } => tool::handle_replay_tool_started(state, tool_id, tool_name, input_summary, raw_input),
        ReplayToolEnded {
            tool_id,
            output_summary,
            is_error,
        } => tool::handle_replay_tool_ended(state, tool_id, output_summary, *is_error),
        // 历史分页：整页并入窗口（本地事件，非 wire；见 `acp_events::history_page`）。
        // 它只动 committed，不碰执行 / 审批 / Todo / 输入状态（REQ-C01-6、A24）。
        HistoryPageLoaded {
            response,
            origin,
            refetch,
        } => history_page::handle_history_page_loaded(
            state,
            response.as_ref(),
            origin.as_ref(),
            refetch.as_deref(),
        ),
        HistoryCommitted(info) => history_window::handle_history_committed(state, info),
        HistoryWindowPageLoaded { context, response } => {
            state.publication_intent = history_window::handle_window_page_loaded(
                state,
                context.as_ref(),
                response.as_ref(),
            );
        }
        HistoryWindowDegraded { session_id, reason } => {
            history_window::handle_window_degraded(state, session_id, reason);
        }
        // 回放条目位置标记：只登记区间，不产生展示单元。
        ReplayEntryMarked { entry_id, position } => {
            crate::kit::history_paging::mark_own_entry(entry_id, *position)
        }

        // ── §4.7 Background Tasks ──
        BgTaskSnapshot {
            tasks,
            revision,
            source,
        } => system::handle_bg_task_snapshot(state, tasks, *revision, *source),
        BgTaskStarted(entry) => system::handle_bg_task_started(state, entry),
        // kind payload 保留（bg task UI 展示 / 未来扩展）；内部续跑由 ACP
        // server 的 continuation scheduler 承担，TUI bridge 不触发 KeepGoing。
        BgTaskCompleted {
            task_id,
            kind: _,
            success,
            duration_ms,
            output_preview,
            revision,
        } => system::handle_bg_task_completed(
            task_id,
            *success,
            *duration_ms,
            output_preview.clone(),
            *revision,
        ),
        BgTaskCancelled {
            task_id,
            reason,
            revision,
        } => system::handle_bg_task_cancelled(task_id, reason, *revision),
        BgTaskUpdated {
            task_id,
            status,
            revision,
        } => system::handle_bg_task_updated(task_id, status, *revision),
    }

    let requested = std::mem::take(&mut state.publication_intent);
    // 回放条目区间：这些变体各自在末尾推入**恰好一个**展示单元，槽位即
    // `committed.len() - 1`。计数不符（例如 user 气泡落进 current_turn，或
    // reminder 前 `flush_current_turn` 并入了流式产出）时不登记，避免把区间错标到
    // 别的条目上。
    //
    // reminder（含 fallback）必须在集合里：首屏自有条目**只有 reminder** 是合法
    // 形状，漏登记会让区间索引为空、更早历史插到继承前缀之前（REQ-H01-6、A20）。
    if state.committed.len() == committed_len_before + 1
        && matches!(
            event,
            CommittedAssistantText { .. }
                | ReplayToolStarted { .. }
                | ReplayedUserBubble { .. }
                | LocalUserBubble { .. }
                | SystemReminder { .. }
                | SystemReminderFallback { .. }
        )
    {
        crate::kit::history_paging::note_own_entry_unit(committed_len_before);
    }
    if requested != PublicationIntent::None {
        requested
    } else if matches!(
        event,
        CommittedAssistantText { .. }
            | ReplayToolStarted { .. }
            | ReplayToolEnded { .. }
            | HistoryPageLoaded { .. }
            | HistoryWindowPageLoaded { .. }
    ) {
        PublicationIntent::Deferred
    } else {
        PublicationIntent::None
    }
}

pub(crate) fn dispatch_trusted_structured_for_bridge(
    state: &mut BridgeState,
    event: AcpEventData,
) -> PublicationIntent {
    match event {
        AcpEventData::SystemReminder {
            reminder,
            source_agent_id,
            ..
        } => {
            system::handle_trusted_system_reminder(state, reminder, source_agent_id.as_deref());
            std::mem::take(&mut state.publication_intent)
        }
        other => dispatch_for_bridge(state, &other),
    }
}

/// 同步测试/非 bridge 调用的兼容入口；生产 bridge 使用 `dispatch_for_bridge`
/// 消费 publication intent 并执行合帧。
#[cfg(test)]
pub(crate) fn dispatch_and_notify(state: &mut BridgeState, event: &AcpEventData) {
    if dispatch_for_bridge(state, event) != PublicationIntent::Published {
        render::push_view_models(state);
    }
    crate::kit::bg_task_live::publish_pending_streams();
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 从 BaseMessage JSON 提取纯文本（H3 辅助函数）。
///
/// content 可能是 string 或 array of {type:text, text:...}，
/// 两种格式都兼容。**仅提取 type=="text" 的 block**——tool_use / tool_result /
/// reasoning / image 等非文本 block 在 rewind 视图下不还原（用户看不到历史工具
/// 调用卡片与 reasoning），这是 display-only 的简化设计，避免在 H3 反序列化
/// 路径重建完整 ViewModel（成本过高且非 rewind 主场景）。
fn extract_message_text(msg: &serde_json::Value) -> String {
    // content 为 string 的简单格式
    if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    // content 为 array 的复杂格式
    if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    block.get("text").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../acp_events_test.rs"]
pub(crate) mod acp_events_test;
