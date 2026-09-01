//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 mpsc::UnboundedReceiver 接收已解码的 ACP 事件，
//! 经 acp_events::dispatch_and_notify 处理后写入全局 Atom。
//! Phase 2 完整实现——main_loop fan-out 后独立消费。

use crate::acp_client::AcpTuiClient;
use crate::kit::acp_events::{self, BridgeState, SessionPhase};
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, CurrentTurn};
use crate::kit::atoms;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// L6: 抽取 BRIDGE_RESET_COUNTER 变更时的 state 重置逻辑，rx.recv() 与
/// tick_interval.tick() 两条分支共用。行为必须与原 rx 分支内联代码完全等价：
/// 更新 last_reset_counter、刷 active_session_id、清空 committed /
/// current_turn / generation / phase / popup_kind / 代际与 request_id、
/// 清 INPUT_BUFFER、push_view_models_for_reset。
fn apply_bridge_reset(state: &mut BridgeState, last_reset_counter: &mut u64, counter: u64) -> u64 {
    let old = *last_reset_counter;
    *last_reset_counter = counter;
    state.active_session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    state.committed = im::Vector::new();
    state.current_turn.reset();
    state.generation = 0;
    state.turn_generation = 0;
    state.last_prompt_generation = 0;
    state.current_request_id = None;
    state.pending_cache_usage = None;
    state.phase = SessionPhase::Idle;
    state.popup_kind = None;
    state.last_submitted_text = None;
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    state.last_successful_todos = None;
    state.last_successful_todo_sequence = None;
    state.next_todo_sequence = 0;
    state.todo_call_inputs.clear();
    atoms::INPUT_BUFFER.state().write().clear();
    acp_events::push_view_models_for_reset();
    tracing::info!(
        old,
        new = counter,
        sid = %state.active_session_id,
        "[CLEAR_DEBUG] bridge: state reset by BRIDGE_RESET_COUNTER"
    );
    old
}

fn accepts_event_session(
    event: &AcpEventData,
    active_session_id: &str,
    incoming_session_id: &str,
    just_reset: bool,
) -> bool {
    if matches!(
        event,
        AcpEventData::HitlPending(_) | AcpEventData::AskUser(_)
    ) {
        return !active_session_id.is_empty()
            && !incoming_session_id.is_empty()
            && active_session_id == incoming_session_id;
    }
    if just_reset {
        incoming_session_id.is_empty() || incoming_session_id == active_session_id
    } else {
        active_session_id.is_empty()
            || incoming_session_id.is_empty()
            || incoming_session_id == active_session_id
    }
}

/// 启动 ACP 事件桥接后台任务。
///
/// 从独立的 mpsc::UnboundedReceiver 读取 ACP 事件（main_loop 会 fan-out），
/// 维护 BridgeState 内部状态，每次事件后写入 VIEW_MODELS / ACP_STATE Atom，
/// 触发 ratatui-kit 组件重渲染。
pub fn spawn_acp_bridge(
    rx: mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    client: AcpTuiClient,
) -> tokio::task::JoinHandle<()> {
    spawn_acp_bridge_inner(rx, shutdown, Some(client), None)
}

fn spawn_acp_bridge_inner(
    mut rx: mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    client: Option<AcpTuiClient>,
    observed: Option<mpsc::UnboundedSender<bool>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
            last_pushed_text_len: 0,
            last_pushed_reasoning_len: 0,
            last_successful_todos: None,
            last_successful_todo_sequence: None,
            next_todo_sequence: 0,
            todo_call_inputs: std::collections::HashMap::new(),
            turn_generation: 0,
            last_prompt_generation: 0,
            current_request_id: None,
            pending_cache_usage: None,
        };

        // 追踪 BRIDGE_RESET_COUNTER——submit_consumer 的 /clear / thread_load
        // 递增此计数器，bridge 检测到变更时立即清空 committed，
        // 防止旧 session 的 ViewModel 在新 session 中残留。
        let mut last_reset_counter: u64 = 0;

        // 每秒检测 BRIDGE_RESET_COUNTER + 刷新 running Bash 计时
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tick_interval.tick() => {
                    // L6: tick 分支也需检测 BRIDGE_RESET_COUNTER——否则 /clear 或
                    // thread_load 在 rx 空闲期递增 counter 时，tick 仍会把旧
                    // committed 写回 VIEW_MODELS，造成旧 session 残留。
                    let counter = atoms::BRIDGE_RESET_COUNTER.get();
                    if counter != last_reset_counter {
                        apply_bridge_reset(&mut state, &mut last_reset_counter, counter);
                        continue;
                    }
                    if state.current_turn.has_running_bash_tool() {
                        state.current_turn.invalidate_cache();
                        use crate::kit::acp_events::current_streaming_mode;
                        use crate::kit::acp_events::StreamingMode;
                        let mode_is_none =
                            matches!(current_streaming_mode(), StreamingMode::None);
                        if !mode_is_none {
                            acp_events::push_view_models(&mut state);
                        }
                    }
                }
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(epoch_event) => {
                            // 先检测 BRIDGE_RESET_COUNTER 变更 → 重置 state
                            // （reset 内部会更新 state.active_session_id，
                            //  因此 session_id filter 必须在 reset 之后执行）
                            let counter = atoms::BRIDGE_RESET_COUNTER.get();
                            let just_reset = counter != last_reset_counter;
                            if just_reset {
                                let old_counter = last_reset_counter;
                                last_reset_counter = counter;
                                state.active_session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
                                state.committed = im::Vector::new();
                                state.current_turn.reset();
                                state.generation = 0;
                                state.turn_generation = 0;
                                state.last_prompt_generation = 0;
                                state.current_request_id = None;
                                state.pending_cache_usage = None;
                                state.phase = SessionPhase::Idle;
                                state.popup_kind = None;
                                state.last_submitted_text = None;
                                state.last_pushed_text_len = 0;
                                state.last_pushed_reasoning_len = 0;
                                state.last_successful_todos = None;
                                state.last_successful_todo_sequence = None;
                                state.next_todo_sequence = 0;
                                state.todo_call_inputs.clear();
                                // 同步清空 INPUT_BUFFER：/clear 和 thread_load 切换时，
                                // 递增 BRIDGE_RESET_COUNTER 触发此分支，旧会话 loading
                                // 期间缓存的输入必须丢弃，防止新会话首个 TurnDone 时
                                // drain_input_buffer() 把旧输入泄漏到新会话。
                                atoms::INPUT_BUFFER.state().write().clear();
                                acp_events::push_view_models_for_reset();
                                tracing::info!(
                                    old = old_counter,
                                    new = counter,
                                    sid = %state.active_session_id,
                                    "[CLEAR_DEBUG] bridge: state reset by BRIDGE_RESET_COUNTER"
                                );
                                // Phase 5 Step 7 补遗（Step 8 回归修复）：
                                // CommandFeedback(UiOnly) 的 compact 完成提示跨
                                // replay 存活——reset 清空 committed/current_turn
                                // 后从 PENDING_COMPACT_NOTE 重建 SystemNote
                                // （机制沿袭 aecc2834，死锁教训：显式块提取值、
                                // guard 立即 drop 后再 set 清空，见 issue
                                // 2026-08-08-e2e-compact-command-screenshot-too-early）。
                                {
                                    let pending = atoms::PENDING_COMPACT_NOTE
                                        .state()
                                        .read()
                                        .clone();
                                    if let Some(text) = pending {
                                        use crate::kit::tui_render_unit::{
                                            TuiNoteLevel, tui_hash_str,
                                        };
                                        state
                                            .current_turn
                                            .push_system_note(text.clone(), TuiNoteLevel::Info, tui_hash_str(&text));
                                        atoms::PENDING_COMPACT_NOTE.set(None);
                                    }
                                }
                            }

                            let interaction_owner = match &epoch_event.event {
                                AcpEventData::HitlPending(pending) => Some(pending.owner.clone()),
                                AcpEventData::AskUser(pending) => Some(pending.owner.clone()),
                                _ => None,
                            };

                            if let (Some(owner), Some(client)) =
                                (interaction_owner, client.as_ref())
                            {
                                let event = epoch_event.event;
                                let published = client
                                    .publish_if_owned(&owner, || {
                                        acp_events::dispatch_and_notify(&mut state, &event)
                                    })
                                    .await;
                                if let Some(tx) = &observed {
                                    let _ = tx.send(published);
                                }
                                continue;
                            }

                            if !accepts_event_session(
                                &epoch_event.event,
                                &state.active_session_id,
                                &epoch_event.active_session_id,
                                just_reset,
                            ) {
                                tracing::debug!(
                                    event_sid = %epoch_event.active_session_id,
                                    state_sid = %state.active_session_id,
                                    "[SESSION_FILTER] dropping unowned event"
                                );
                                if let Some(tx) = &observed {
                                    let _ = tx.send(false);
                                }
                                continue;
                            }

                            let event = epoch_event.event;

                            // === [CLEAR_DEBUG] 诊断 instrumentation（临时） ===
                            // 目的：定位 /clear 后哪个事件把旧数据写回 committed。
                            // 仅在状态变化或刚 reset 时打印，避免日志爆炸。
                            let event_kind = event_kind_short(&event);
                            let committed_before = state.committed.len();
                            let current_turn_before = state.current_turn.view_models().len();

                            acp_events::dispatch_and_notify(&mut state, &event);

                            let committed_after = state.committed.len();
                            let current_turn_after = state.current_turn.view_models().len();

                            if committed_after != committed_before
                                || current_turn_after != current_turn_before
                                || just_reset
                            {
                                tracing::info!(
                                    event_kind,
                                    committed_before,
                                    committed_after,
                                    current_turn_before,
                                    current_turn_after,
                                    just_reset,
                                    generation = state.generation,
                                    "[CLEAR_DEBUG] dispatch event"
                                );
                            }
                            if let Some(tx) = &observed {
                                let _ = tx.send(true);
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
fn spawn_acp_bridge_observed(
    rx: mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    observed: mpsc::UnboundedSender<bool>,
) -> tokio::task::JoinHandle<()> {
    spawn_acp_bridge_inner(rx, shutdown, None, Some(observed))
}

#[cfg(test)]
pub(crate) fn spawn_acp_bridge_observed_with_client(
    rx: mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    client: AcpTuiClient,
    observed: mpsc::UnboundedSender<bool>,
) -> tokio::task::JoinHandle<()> {
    spawn_acp_bridge_inner(rx, shutdown, Some(client), Some(observed))
}

#[cfg(test)]
#[path = "acp_bridge_test.rs"]
mod tests;

/// [CLEAR_DEBUG] 诊断 helper：返回 AcpEventData 变体的短名字。
///
/// 临时 instrumentation——避免每条日志打印完整 event 内容。定位到 /clear 后
/// 污染 committed 的事件类型后即可移除。
fn event_kind_short(event: &AcpEventData) -> &'static str {
    use AcpEventData::*;
    match event {
        TextChunk(_) => "TextChunk",
        ReasoningChunk(_) => "ReasoningChunk",
        ToolStarted(_) => "ToolStarted",
        ToolEnded(_) => "ToolEnded",
        PromptStarted => "PromptStarted",
        PromptSubmitted { .. } => "PromptSubmitted",
        CacheUsageUpdated(_) => "CacheUsageUpdated",
        SessionReplayStarted => "SessionReplayStarted",
        SessionReplayDone => "SessionReplayDone",
        TurnDone => "TurnDone",
        TurnInterrupted { .. } => "TurnInterrupted",
        TurnSuspended => "TurnSuspended",
        LocalUserBubble { .. } => "LocalUserBubble",
        LocalLoadingReset => "LocalLoadingReset",
        BgCallbackBubble { .. } => "BgCallbackBubble",
        CommittedAssistantText { .. } => "CommittedAssistantText",
        ReplayToolStarted { .. } => "ReplayToolStarted",
        ReplayToolEnded { .. } => "ReplayToolEnded",
        ToolCount(_) => "ToolCount",
        Progress(_) => "Progress",
        BudgetWarning(_) => "BudgetWarning",
        SystemNotification(_) => "SystemNotification",
        CommandFeedback(_) => "CommandFeedback",
        Prediction(_) => "Prediction",
        FileSuggestions(_) => "FileSuggestions",
        HitlPending(_) => "HitlPending",
        AskUser(_) => "AskUser",
        InteractionTerminal { .. } => "InteractionTerminal",
        RewindPreview(_) => "RewindPreview",
        OauthNeeded(_) => "OauthNeeded",
        OauthCompleted { .. } => "OauthCompleted",
        OauthFailed { .. } => "OauthFailed",
        OauthRestored { .. } => "OauthRestored",
        SubagentStarted { .. } => "SubagentStarted",
        SubagentStopped { .. } => "SubagentStopped",
        Unknown { .. } => "Unknown",
        BgTaskStarted(_) => "BgTaskStarted",
        BgTaskCompleted { .. } => "BgTaskCompleted",
        BgTaskCancelled { .. } => "BgTaskCancelled",
        BgTaskSnapshot(_) => "BgTaskSnapshot",
        TurnCommitted { .. } => "TurnCommitted",
        CompactStarted => "CompactStarted",
        CompactCompleted { .. } => "CompactCompleted",
        BackgroundTaskCompleted { .. } => "BackgroundTaskCompleted",
        LlmRetrying { .. } => "LlmRetrying",
        AgentExecutionFailed { .. } => "AgentExecutionFailed",
        WorkflowProgress { .. } => "WorkflowProgress",
        RewindCompleted { .. } => "RewindCompleted",
        PluginSnapshot(_) => "PluginSnapshot",
        PluginActionResult(_) => "PluginActionResult",
        PluginSearchResult(_) => "PluginSearchResult",
    }
}
