//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 mpsc::UnboundedReceiver 接收已解码的 ACP 事件，
//! 经 acp_events::dispatch_and_notify 处理后写入全局 Atom。
//! Phase 2 完整实现——main_loop fan-out 后独立消费。

use crate::kit::acp_events::{self, BridgeState};
use crate::kit::acp_types::{AcpEventData, CurrentTurn};
use crate::kit::atoms;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 启动 ACP 事件桥接后台任务。
///
/// 从独立的 mpsc::UnboundedReceiver 读取 ACP 事件（main_loop 会 fan-out），
/// 维护 BridgeState 内部状态，每次事件后写入 VIEW_MODELS / ACP_STATE Atom，
/// 触发 ratatui-kit 组件重渲染。
pub fn spawn_acp_bridge(
    mut rx: mpsc::UnboundedReceiver<AcpEventData>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
        };

        // 追踪 BRIDGE_RESET_COUNTER——submit_consumer 的 /clear / thread_load
        // 递增此计数器，bridge 检测到变更时立即清空 committed/has_turn_done，
        // 防止旧 session 的 ViewModel 在新 session 中残留。
        let mut last_reset_counter: u64 = 0;

        // 每秒触发 spinner 推进 + 耗时刷新（含 running Bash 的 Running(Ns) 计时器）
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tick_interval.tick() => {
                    state.current_turn.advance_spinner();
                    if state.current_turn.has_running_bash_tool() {
                        acp_events::push_view_models(&mut state);
                    }
                }
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(event) => {
                            let counter = atoms::BRIDGE_RESET_COUNTER.get();
                            let just_reset = counter != last_reset_counter;
                            if just_reset {
                                let old_counter = last_reset_counter;
                                last_reset_counter = counter;
                                state.committed = Arc::from([]);
                                state.current_turn.reset();
                                state.has_turn_done = false;
                                state.is_loading = false;
                                state.popup_kind = None;
                                // 同步清空 INPUT_BUFFER：/clear 和 thread_load 切换时，
                                // 递增 BRIDGE_RESET_COUNTER 触发此分支，旧会话 loading
                                // 期间缓存的输入必须丢弃，防止新会话首个 TurnDone 时
                                // drain_input_buffer() 把旧输入泄漏到新会话。
                                atoms::INPUT_BUFFER.state().write().clear();
                                acp_events::push_view_models_for_reset();
                                tracing::info!(
                                    old = old_counter,
                                    new = counter,
                                    "[CLEAR_DEBUG] bridge: state reset by BRIDGE_RESET_COUNTER"
                                );
                            }

                            // === [CLEAR_DEBUG] 诊断 instrumentation（临时） ===
                            // 目的：定位 /clear 后哪个事件把旧数据写回 committed。
                            // 仅在状态变化或刚 reset 时打印，避免日志爆炸。
                            let event_kind = event_kind_short(&event);
                            let committed_before = state.committed.len();
                            let current_turn_before = state.current_turn.view_models().len();
                            let has_turn_done_before = state.has_turn_done;

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
                                    has_turn_done_before,
                                    has_turn_done_after = state.has_turn_done,
                                    just_reset,
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
        TurnDone => "TurnDone",
        TurnInterrupted { reason: _ } => "TurnInterrupted",
        ReplayUserBubble { .. } => "ReplayUserBubble",
        ReplayAssistantBubble { .. } => "ReplayAssistantBubble",
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
    }
}
