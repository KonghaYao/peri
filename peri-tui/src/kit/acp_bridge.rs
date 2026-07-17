//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 mpsc::UnboundedReceiver 接收已解码的 ACP 事件，
//! 经 acp_events::dispatch_and_notify 处理后写入全局 Atom。
//! Phase 2 完整实现——main_loop fan-out 后独立消费。

use crate::kit::acp_events::{self, BridgeState, SessionPhase};
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, CurrentTurn};
use crate::kit::atoms;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// L6: 抽取 BRIDGE_RESET_COUNTER 变更时的 state 重置逻辑，rx.recv() 与
/// tick_interval.tick() 两条分支共用。行为必须与原 rx 分支内联代码完全等价：
/// 更新 last_reset_counter、刷 active_session_id、清空 committed /
/// current_turn / generation / phase / popup_kind、清 INPUT_BUFFER、
/// push_view_models_for_reset。
fn apply_bridge_reset(state: &mut BridgeState, last_reset_counter: &mut u64, counter: u64) -> u64 {
    let old = *last_reset_counter;
    *last_reset_counter = counter;
    state.active_session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    state.committed = im::Vector::new();
    state.current_turn.reset();
    state.generation = 0;
    state.phase = SessionPhase::Idle;
    state.popup_kind = None;
    state.last_submitted_text = None;
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

/// 启动 ACP 事件桥接后台任务。
///
/// 从独立的 mpsc::UnboundedReceiver 读取 ACP 事件（main_loop 会 fan-out），
/// 维护 BridgeState 内部状态，每次事件后写入 VIEW_MODELS / ACP_STATE Atom，
/// 触发 ratatui-kit 组件重渲染。
pub fn spawn_acp_bridge(
    mut rx: mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    shutdown: CancellationToken,
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
                        acp_events::push_view_models(&mut state);
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
                                state.phase = SessionPhase::Idle;
                                state.popup_kind = None;
                                state.last_submitted_text = None;
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
                                // reset 触发事件可能来自旧 session——此时需过滤
                                // （state.active_session_id 已更新为新值，旧事件不匹配）
                                if !epoch_event.active_session_id.is_empty()
                                    && epoch_event.active_session_id != state.active_session_id
                                {
                                    tracing::debug!(
                                        event_sid = %epoch_event.active_session_id,
                                        state_sid = %state.active_session_id,
                                        "[SESSION_FILTER] dropping stale event that triggered reset"
                                    );
                                    continue;
                                }
                            }

                            // 非 reset 路径：陈旧事件过滤（active_session_id 不匹配 → 丢弃）。
                            // 仅当 state 已初始化（active_session_id 非空）时才过滤——
                            // state.active_session_id 为空意味着 bridge 尚未确认
                            // 当前活跃 session（entry.rs 初始会话创建前），此时不应
                            // 丢弃任何事件，否则 ACP 应答事件全部被过滤，渲染管线断流。
                            if !just_reset
                                && !state.active_session_id.is_empty()
                                && !epoch_event.active_session_id.is_empty()
                                && epoch_event.active_session_id != state.active_session_id
                            {
                                tracing::debug!(
                                    event_sid = %epoch_event.active_session_id,
                                    state_sid = %state.active_session_id,
                                    "[SESSION_FILTER] dropping stale event from old session"
                                );
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
                        }
                    }
                }
            }
        }
    })
}

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
        SessionReplayStarted => "SessionReplayStarted",
        SessionReplayDone => "SessionReplayDone",
        TurnDone => "TurnDone",
        TurnInterrupted { reason: _ } => "TurnInterrupted",
        TurnSuspended => "TurnSuspended",
        LocalUserBubble { .. } => "LocalUserBubble",
        BgCallbackBubble { .. } => "BgCallbackBubble",
        CommittedAssistantText { .. } => "CommittedAssistantText",
        ReplayToolStarted { .. } => "ReplayToolStarted",
        ReplayToolEnded { .. } => "ReplayToolEnded",
        ToolCount(_) => "ToolCount",
        Progress(_) => "Progress",
        BudgetWarning(_) => "BudgetWarning",
        SystemNotification(_) => "SystemNotification",
        Prediction(_) => "Prediction",
        FileSuggestions(_) => "FileSuggestions",
        HitlPending(_) => "HitlPending",
        AskUser(_) => "AskUser",
        RewindPreview(_) => "RewindPreview",
        OauthNeeded(_) => "OauthNeeded",
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
        CompactError { .. } => "CompactError",
        BackgroundTaskCompleted { .. } => "BackgroundTaskCompleted",
        AgentExecutionFailed { .. } => "AgentExecutionFailed",
        WorkflowProgress { .. } => "WorkflowProgress",
        RewindCompleted { .. } => "RewindCompleted",
        PluginSnapshot(_) => "PluginSnapshot",
        PluginActionResult(_) => "PluginActionResult",
        PluginSearchResult(_) => "PluginSearchResult",
    }
}
