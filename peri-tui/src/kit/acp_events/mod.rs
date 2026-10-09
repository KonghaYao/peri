//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

// ── Sub-modules ──
mod agent;
mod compact;
mod fold;
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
use crate::kit::tui_render_unit::{TuiNoteLevel, tui_hash_str};
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
    /// `/compact` 命令刚刚完成，TurnDone 时需触发 session/load 重放。
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

impl BridgeState {
    /// 显式同步 barrier：保留 terminal 发布先于 drain/replay 等副作用的顺序。
    fn publish_barrier(&mut self) {
        render::push_view_models(self);
        self.publication_intent = PublicationIntent::Published;
    }

    /// 将 current_turn 已产出内容 flush 到 committed，然后 reset。
    ///
    /// 用于 BgCallbackBubble / TurnDone 两个需要保证时序正确性的位置：
    /// 在 push 中间气泡或结束当前 turn 之前，必须先将已产出的 AI 内容归档到
    /// committed，确保消息流时间顺序一致。
    ///
    /// 安全守卫：如果 current_turn 中存在正在运行的 SubAgentAccumulator，
    /// 跳过 flush 以避免清除容器——否则后续工具事件无法路由到已清除的容器，
    /// 造成 SubAgentGroup 内部卡片空白（具体现象：外壳可见但内部工具条目缺失）。
    ///
    /// 注意：SystemNote（BudgetWarning/SystemNotification/CommandFeedback/
    /// AgentExecutionFailed）不再通过 flush-then-push 模式，
    /// 改为 inject_system_note() 统一注入 current_turn 内部 segment。
    fn flush_current_turn(&mut self) {
        if !self.current_turn.committed && !self.current_turn.is_empty() {
            let has_running_subagent = self.current_turn.subagents.iter().any(|s| s.is_running);
            if has_running_subagent {
                tracing::debug!(
                    running_count = self
                        .current_turn
                        .subagents
                        .iter()
                        .filter(|s| s.is_running)
                        .count(),
                    "flush_current_turn: 跳过 flush——存在正在运行的 SubAgentAccumulator"
                );
                return;
            }
            // 与 TurnInterrupted/TurnSuspended 一致：归档前 deactivate，避免
            // ToolStarted 无 ToolEnded（如工具不存在）时 running 态被写入 committed。
            self.current_turn.deactivate();
            for vm in self.current_turn.view_models() {
                self.committed.push_back(vm.clone());
            }
        }
        self.current_turn.reset();
    }

    /// Put a user prompt after all output already received from the preceding
    /// turn. A live subagent keeps its container in current_turn, so in that
    /// case the prompt joins the same chronological segment stream.
    fn push_user_bubble(&mut self, text: String) {
        self.flush_current_turn();
        if self.current_turn.is_empty() {
            self.committed
                .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text)));
        } else {
            self.current_turn.push_user_bubble(text);
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
    use AcpEventData::*;
    // S4.1 方案 B：CompactCompleted 置 compact_just_completed 后，任何流事件
    // 到达即清除标志——agent 内部 auto-compact 后 ReAct 循环继续产出，流事件
    // （TextChunk/ReasoningChunk/ToolStarted/ToolEnded/SubagentStarted/
    // ToolCount/Progress）必然紧随；命令 /compact（Immediate）后无流事件，
    // 标志保持到 TurnDone 触发 session/load 重放。无需知道 compact 触发来源
    // （Issue 2026-08-05）。清单与 issue 声明清单保持同步（含 ToolCount/Progress）。
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
        SessionReplayDone => turn::handle_session_replay_done(state),
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
        SystemReminder { reminder, .. } => system::handle_system_reminder(state, reminder),
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
        LocalUserBubble { text } => turn::handle_local_user_bubble(state, text),
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
            turn::handle_local_user_bubble(state, text);
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

        // ── §4.7 Background Tasks ──
        BgTaskSnapshot { tasks, revision } => {
            system::handle_bg_task_snapshot(state, tasks, *revision)
        }
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
    if requested != PublicationIntent::None {
        requested
    } else if matches!(
        event,
        CommittedAssistantText { .. } | ReplayToolStarted { .. } | ReplayToolEnded { .. }
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
        AcpEventData::SystemReminder { reminder, .. } => {
            system::handle_trusted_system_reminder(state, reminder);
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
mod acp_events_test;
