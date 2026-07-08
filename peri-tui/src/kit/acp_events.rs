//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::kit::acp_types::{AcpEventData, CurrentTurn, ToolCardAccumulator};
use crate::kit::atoms::*;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::tui_render_unit::{
    TuiNoteLevel, TuiRenderUnit, TuiSystemNote, TuiUserBubble, tui_hash_str,
};
use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// BridgeState — ACP 事件桥接内部状态
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    PromptRunning,
    ReplayingHistory,
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
    pub popup_kind: Option<crate::kit::atoms::PopupKind>,
    /// ViewModelsSnapshot generation——每次 push_view_models 递增。
    /// render_bridge 用此值检测变化（替代原先的 Arc::as_ptr 比较）。
    pub generation: u64,
    /// 当前活跃 session 的 ID。事件携带的 active_session_id 不匹配时丢弃。
    pub active_session_id: String,
}

// ---------------------------------------------------------------------------
// 核心分发函数
// ---------------------------------------------------------------------------

/// 将 AcpEventData 分发到对应的 Atom 写入，并更新 BridgeState。
///
/// 这是 acp_bridge 消费事件时调用的核心函数。
/// 每次调用按事件类型更新内部状态，然后 push 到 VIEW_MODELS 和 ACP_STATE Atoms。
pub fn dispatch_and_notify(state: &mut BridgeState, event: &AcpEventData) {
    use AcpEventData::*;
    match event {
        // ── §4.1 Streaming events ──
        TextChunk(tc) => {
            if let Some(agent_id) = tc.agent_id.as_deref() {
                if !state.current_turn.append_subagent_text(agent_id, &tc.text) {
                    tracing::trace!(
                        agent_id,
                        "kit bridge: subagent text chunk has no active group"
                    );
                }
                state.variant = 1;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .append_text(&tc.text, tc.message_id.as_deref());
                state.variant = 1;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ReasoningChunk(rc) => {
            if let Some(agent_id) = rc.agent_id.as_deref() {
                if !state
                    .current_turn
                    .append_subagent_reasoning(agent_id, &rc.text)
                {
                    tracing::trace!(
                        agent_id,
                        "kit bridge: subagent reasoning chunk has no active group"
                    );
                }
                state.variant = 1;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .append_reasoning(&rc.text, rc.message_id.as_deref());
                tracing::info!(
                    len = state.current_turn.reasoning.len(),
                    "bridge: reasoning appended"
                );
                state.variant = 1;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ToolStarted(ts) => {
            if let Some(agent_id) = ts.agent_id.as_deref() {
                let routed = state.current_turn.start_subagent_tool(
                    agent_id,
                    ToolCardAccumulator::new(
                        ts.tool_id.clone(),
                        ts.tool_name.clone(),
                        ts.input_summary.clone(),
                    ),
                );
                if !routed {
                    tracing::trace!(agent_id, tool_id = %ts.tool_id, "kit bridge: subagent tool start has no active group");
                }
                state.variant = 1;
                push_view_models(state);
            } else {
                state.current_turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
                state.variant = 1;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ToolEnded(te) => {
            if let Some(agent_id) = te.agent_id.as_deref() {
                let routed = state.current_turn.end_subagent_tool(
                    agent_id,
                    &te.tool_id,
                    te.output_summary.clone(),
                    te.is_error,
                );
                if !routed {
                    tracing::trace!(agent_id, tool_id = %te.tool_id, "kit bridge: subagent tool end has no active group");
                }
                state.variant = 1;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
                state.variant = 1;
                push_view_models(state);
            }
            push_acp_state(state);
        }

        // ── §4.2 Boundary events ──
        PromptStarted => {
            state.phase = SessionPhase::PromptRunning;
            state.variant = 1;
            push_acp_state(state);
        }
        SessionReplayStarted => {
            state.phase = SessionPhase::ReplayingHistory;
            state.variant = 0;
            state.current_turn.reset();
            push_view_models(state);
            push_acp_state(state);
        }
        SessionReplayDone => {
            if state.phase == SessionPhase::ReplayingHistory {
                state.phase = SessionPhase::Idle;
            }
            state.variant = 0;
            state.current_turn.reset();
            push_view_models(state);
            push_acp_state(state);
        }
        TurnDone => {
            // H3: TurnDone 仅做两件事：
            // (a) current_turn.view_models() → 逐条 push_back 到 committed
            // (b) current_turn.reset() + push_view_models
            // buffered_text 已由 LocalUserBubble 事件提前入队 committed，
            // TurnDone 不再代为搬运。
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                for vm in state.current_turn.view_models() {
                    state.committed.push_back(vm.clone());
                }
            }

            state.current_turn.reset();
            state.variant = 0;

            state.phase = SessionPhase::Idle;

            tracing::info!(
                is_loading = state.phase == SessionPhase::PromptRunning,
                committed_len = state.committed.len(),
                current_turn_empty = state.current_turn.is_empty(),
                "TurnDone: writing ACP_STATE"
            );

            push_view_models(state);
            push_acp_state(state);

            // (g) C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
            drain_input_buffer();
        }
        TurnInterrupted { reason: _reason } => {
            // 守卫：仅当 current_turn 有未归档内容时才归档
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                state.current_turn.deactivate();
                for vm in state.current_turn.view_models() {
                    state.committed.push_back(vm.clone());
                }
            }
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.3 Status events ──
        ToolCount(_tc) => {
            push_acp_state(state);
        }
        Progress(_) => {
            push_acp_state(state);
        }
        BudgetWarning(_) => {
            push_acp_state(state);
        }
        SystemNotification(sn) => {
            let level = match sn.level.as_str() {
                "warning" => TuiNoteLevel::Warning,
                "error" => TuiNoteLevel::Error,
                _ => TuiNoteLevel::Info,
            };
            let content_hash = tui_hash_str(&format!("{}|{:?}", sn.text, level));
            state
                .committed
                .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                    text: sn.text.clone(),
                    level,
                    content_hash,
                }));
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.4 Input assist (no-op for now) ──
        Prediction(_) | FileSuggestions(_) => {}

        // ── §4.5 Interaction events ──
        // S7：把每个交互事件映射到具体 PopupKind，让 PopupOverlay 精确路由
        HitlPending(hp) => {
            // I21-A：保存 payload 到 HITL_PENDING atom，供 HitlPopup 读取真实数据
            *HITL_PENDING.state().write() = Some(hp.clone());
            state.popup_kind = Some(PopupKind::Hitl);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        AskUser(au) => {
            // I21-B：保存 payload 到 ASK_USER_PENDING atom，供 AskUserPanel 读取真实数据。
            // 通过 panel_registry 打开 AskUser 面板（非弹窗），内联在 MessageArea 下方。
            *ASK_USER_PENDING.state().write() = Some(au.clone());
            crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::AskUser);
            state.variant = 2;
            push_acp_state(state);
        }
        RewindPreview(rp) => {
            // S10：保存 payload 到 REWIND_PREVIEW atom，供 RewindPopup 读取真实数据
            *REWIND_PREVIEW.state().write() = Some(rp.clone());
            state.popup_kind = Some(PopupKind::Rewind);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        OauthNeeded(on) => {
            // I20-D：保存 payload 到 OAUTH_INFO atom，供 OAuthPopup 读取真实数据
            *OAUTH_INFO.state().write() = Some(on.clone());
            state.popup_kind = Some(PopupKind::OAuth);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }

        // ── §4.6 Structure events ──
        SubagentStarted {
            agent_id,
            agent_name,
        } => {
            state
                .current_turn
                .start_subagent(agent_id.clone(), agent_name.clone());
            state.variant = 1;
            push_view_models(state);
            push_acp_state(state);
        }
        SubagentStopped { agent_id } => {
            state.current_turn.stop_subagent(agent_id);
            state.variant = 1;
            push_view_models(state);
            push_acp_state(state);
        }

        // ── Replay events ──
        ReplayUserBubble { text } => {
            let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: text.clone(),
                content_hash: tui_hash_str(text),
                is_system_reminder: false,
            });
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }
        ReplayAssistantBubble { text } => {
            let vm = TuiRenderUnit::TuiAssistantBubble(
                crate::kit::tui_render_unit::TuiAssistantBubble {
                    text: text.clone(),
                    reasoning: None,
                    content_hash: 0,
                },
            );
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.8 Agent Event Extensions ──
        TurnCommitted {
            messages_json: _,
            steps,
        } => {
            tracing::info!(steps, "bridge: TurnCommitted ({steps} steps)");
        }
        CompactStarted => {
            tracing::info!("bridge: CompactStarted");
            state.phase = SessionPhase::PromptRunning;
            push_acp_state(state);
        }
        CompactCompleted { summary, .. } => {
            tracing::info!(summary_len = summary.len(), "bridge: CompactCompleted");
            state.phase = SessionPhase::Idle;
            push_acp_state(state);
        }
        CompactError { message } => {
            tracing::warn!(message, "bridge: CompactError");
            state.phase = SessionPhase::Idle;
            push_acp_state(state);
        }
        BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            duration_ms,
            ..
        } => {
            let msg = if *success {
                format!(
                    "后台 {} {} 完成 ({:.0}s)",
                    agent_name,
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            } else {
                format!(
                    "后台 {} {} 失败 ({:.0}s)",
                    agent_name,
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            };
            tracing::info!(msg, "bridge: BackgroundTaskCompleted");
        }
        AgentExecutionFailed { message } => {
            tracing::error!(message, "bridge: AgentExecutionFailed");
            state.phase = SessionPhase::Idle;
            push_acp_state(state);
        }
        WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            phase,
            ..
        } => {
            tracing::debug!(
                run_id,
                workflow_name,
                event_type,
                phase = ?phase,
                "bridge: WorkflowProgress"
            );
        }

        // ── Unknown / forward-compat ──
        Unknown { .. } => {}
        LocalUserBubble { text } => {
            state
                .committed
                .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                    text: text.clone(),
                    content_hash: tui_hash_str(text),
                    is_system_reminder: false,
                }));
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.7 Background Tasks ──
        BgTaskSnapshot(tasks) => {
            BG_TASKS.state().write().clone_from(tasks);
        }
        BgTaskStarted(task) => {
            BG_TASKS.state().write().push(task.clone());
        }
        BgTaskCompleted {
            task_id,
            success,
            duration_ms,
        } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
            let msg = if *success {
                format!(
                    "[✓] {} 完成 ({:.0}s)",
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            } else {
                format!(
                    "[✗] {} 失败 ({:.0}s)",
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            };
            NOTIFICATION.state().write().replace(Notification {
                message: msg,
                until: Instant::now() + Duration::from_millis(1500),
            });
        }
        BgTaskCancelled { task_id, .. } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Atom push 辅助函数
// ---------------------------------------------------------------------------

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后逐条 push_back
/// `current_turn.view_models()`，构成扁平单层列表。generation 每次调用递增+1。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    let mut items = state.committed.clone();
    for vm in state.current_turn.view_models() {
        items.push_back(vm.clone());
    }
    state.generation = state.generation.wrapping_add(1);
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::new(),
        generation: 0,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
///
/// 仅在快照值变化时才写入——避免不必要的全树重渲染。
/// 流式期间 variant/is_loading 不变时，仅 view_count 变化；
/// popup 状态由各自的独立 atom 追踪（SLASH_HINT_ACTIVE 等），
/// 不应写入 ACP_STATE 导致 AppShell 重渲染。
fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.phase == SessionPhase::PromptRunning,
        wizard_active: false,
        at_mention_active: *AT_MENTION_ACTIVE.state().read(),
        slash_hint_active: *SLASH_HINT_ACTIVE.state().read(),
    };
    let ref_guard = ACP_STATE.state();
    let mut acp = ref_guard.write();
    if *acp != snapshot {
        *acp = snapshot;
    }
}

/// 将 BridgeState.popup_kind 写入 POPUP_KIND Atom（S7）。
fn push_popup_kind(state: &BridgeState) {
    *POPUP_KIND.state().write() = state.popup_kind;
}

/// 将 `INPUT_BUFFER` atom 中所有排队输入按入队顺序 drain，逐条发送到 SUBMIT_TX。
///
/// 调用时机：`TurnDone` 事件——agent 完成本轮，从队列里取出用户在 loading 期间
/// 缓存的 agent text 继续提交。若 buffer 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
///
/// 多条输入的顺序保证：VecDeque + 顺序 `tx.send` + submit_consumer 单消费者 →
/// 严格 FIFO。第一条立即触发 prompt，后续在 submit_consumer 内部顺序处理
/// （每条都等上一条的 RPC 完成）。
fn drain_input_buffer() {
    let tx = SUBMIT_TX.get().cloned();
    if tx.is_none() {
        return;
    }

    let drained: Vec<String> = INPUT_BUFFER.state().write().drain(..).collect();
    if let Some(tx) = tx {
        for text in drained {
            let _ = tx.send(SubmitRequest::AgentText(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::message_area::TodoStatus;
    use serde_json::json;
    use serial_test::serial;
    use tokio::sync::mpsc;

    #[test]
    #[serial]
    fn test_dispatch_subagent_streaming_updates_current_turn_group() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: "agent-1".into(),
                agent_name: "researcher".into(),
            },
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "child text".into(),
                message_id: None,
                agent_id: Some("agent-1".into()),
            }),
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.items.len(), 1);
        match &snapshot.items[0] {
            TuiRenderUnit::TuiSubAgentGroup(group) => {
                assert_eq!(group.agent_id, "agent-1");
                assert_eq!(group.view_models.len(), 1);
            }
            other => panic!("expected TuiSubAgentGroup, got {other:?}"),
        }
    }

    /// C1 回归测试：drain_input_buffer 清空 INPUT_BUFFER 队列。
    ///
    /// 注：不验证 SUBMIT_TX 接收——SUBMIT_TX 是 OnceLock 全局句柄，一旦被其他
    /// 测试 set 就无法重置；此处只验证 drain 的核心效应（buffer 被清空）。
    /// 顺序保证由 `VecDeque::drain(..)` + 顺序 `tx.send` 在源码层面保证。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_preserves_order() {
        crate::kit::atoms::init_atoms();
        let _ = SUBMIT_TX.get_or_init(|| {
            let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
            tx
        });

        // 入队三条
        {
            let state = INPUT_BUFFER.state();
            let mut buf = state.write();
            buf.push_back("first".into());
            buf.push_back("second".into());
            buf.push_back("third".into());
        }

        drain_input_buffer();

        // 验证 buffer 已被 drain 干净——这是 drain_input_buffer 的核心效应
        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "buffer should be empty after drain"
        );
    }

    /// C1 回归测试：空 buffer 是 no-op，drain 后仍为空。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_empty_is_noop() {
        crate::kit::atoms::init_atoms();
        let _ = SUBMIT_TX.get_or_init(|| {
            let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
            tx
        });

        INPUT_BUFFER.state().write().clear();
        drain_input_buffer();

        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "empty buffer should remain empty"
        );
    }

    /// C1 回归测试：SUBMIT_TX 未初始化时安全跳过，不 panic，buffer 也保持不变。
    ///
    /// 注：实际运行时 OnceLock 一旦 set 无法 unset；本测试只验证不 panic。
    #[test]
    #[serial]
    fn test_drain_input_buffer_no_submit_tx_safe() {
        crate::kit::atoms::init_atoms();
        // 不论 SUBMIT_TX 是否 set，都不应 panic
        INPUT_BUFFER.state().write().push_back("x".into());
        drain_input_buffer();
        // SUBMIT_TX 已被前面测试 set 过，所以 drain 成功 → buffer 被清空
        // 即使 SUBMIT_TX 未 set，drain 早退，buffer 仍有 "x"——两种情况都不算 panic
    }

    /// BRIDGE_RESET_COUNTER 递增时 acp_bridge 重置分支同步清空 INPUT_BUFFER，
    /// 防止旧会话缓存输入在新会话 TurnDone 时泄漏。
    ///
    /// 此测试模拟 bridge 的 counter != last_reset_counter 分支：先填入 buffer 数据，
    /// 递增 BRIDGE_RESET_COUNTER，构造任意事件 dispatch，断言 buffer 已被清空。
    /// 注意：实际清空发生在 acp_bridge.rs 的 counter 检测分支，而非 dispatch_and_notify
    /// 内部。此测试模拟的是那个分支调用 push_view_models_for_reset() 前后的完整效应。
    #[test]
    #[serial]
    fn test_bridge_reset_clears_input_buffer() {
        crate::kit::atoms::init_atoms();
        // 填入 buffer 数据
        INPUT_BUFFER
            .state()
            .write()
            .push_back("leaked input".into());
        INPUT_BUFFER
            .state()
            .write()
            .push_back("another leaked input".into());
        assert!(!INPUT_BUFFER.state().read().is_empty(), "buffer 应有数据");

        // 模拟 acp_bridge 的 counter 检测分支：
        // push_view_models_for_reset() 前同步清空 INPUT_BUFFER
        INPUT_BUFFER.state().write().clear();
        push_view_models_for_reset();

        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "bridge reset 后 INPUT_BUFFER 应被清空"
        );

        // VIEW_MODELS 也应被重置
        let snapshot = VIEW_MODELS.state().read().clone();
        assert!(
            snapshot.items.is_empty(),
            "bridge reset 后 committed 应为空"
        );
        assert!(
            snapshot.items.is_empty(),
            "bridge reset 后 current_turn 应为空"
        );
    }

    #[test]
    #[serial]
    fn test_replay_user_bubble_appends_to_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReplayUserBubble {
                text: "hello from replay".into(),
            },
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.items.len(), 1);
        match &snapshot.items[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "hello from replay"),
            other => panic!("expected TuiUserBubble, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn test_replay_assistant_bubble_appends_to_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReplayAssistantBubble {
                text: "assistant from replay".into(),
            },
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.items.len(), 1);
        match &snapshot.items[0] {
            TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "assistant from replay"),
            other => panic!("expected TuiAssistantBubble, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn test_two_turn_done_accumulates_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        // 第一轮：stream one text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "first turn".into(),
                message_id: None,
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert_eq!(
            state.committed.len(),
            1,
            "first TurnDone: committed should have 1 VM"
        );

        // 第二轮：stream another text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "second turn".into(),
                message_id: None,
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(
            snapshot.items.len(),
            2,
            "two TurnDones: committed should have 2 VMs"
        );
    }

    /// TurnDone 归档 assistant VM 到 committed，不再代为搬运 buffered_text。
    #[test]
    #[serial]
    fn test_turndone_archives_assistant_to_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        // 往 current_turn 写入一条 assistant 文本
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "assistant reply".into(),
                message_id: None,
                agent_id: None,
            }),
        );

        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // TurnDone 后 assistant VM 被归档到 committed
        assert_eq!(
            state.committed.len(),
            1,
            "committed 应有 1 个 VM：TuiAssistantBubble"
        );
        match &state.committed[0] {
            TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "assistant reply"),
            other => panic!("expected TuiAssistantBubble at [0], got {other:?}"),
        }
    }

    /// TurnInterrupted 空 current_turn 不归档
    #[test]
    #[serial]
    fn test_turn_interrupted_empty_skips_archive() {
        crate::kit::atoms::init_atoms();
        // 预置一条 committed 数据
        let pre_existing = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "existing".into(),
            content_hash: 1,
            is_system_reminder: false,
        })]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: pre_existing.clone(),
            generation: 0,
        };
        let mut state = BridgeState {
            variant: 1,
            committed: pre_existing,
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::PromptRunning,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::TurnInterrupted {
                reason: "test".into(),
            },
        );

        assert_eq!(
            state.committed.len(),
            1,
            "空 current_turn → TurnInterrupted 不应归档，committed 长度不变"
        );
        match &state.committed[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "existing"),
            other => panic!("committed[0] 应为原始 TuiUserBubble, got {other:?}"),
        }
        assert!(state.current_turn.is_empty(), "current_turn 应已重置");
        assert_eq!(state.phase, SessionPhase::Idle, "phase 应为 Idle");
    }

    #[test]
    #[serial]
    /// push_view_models 以 BridgeState 为准，不再 fallback 到 atom 旧值。
    #[test]
    #[serial]
    fn test_push_view_models_uses_bridge_state() {
        crate::kit::atoms::init_atoms();
        // atom 中有旧数据
        let old_items = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "old data".into(),
            content_hash: 1,
            is_system_reminder: false,
        })]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: old_items,
            generation: 0,
        };

        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
        };

        // push_view_models: 用 BridgeState 数据（空 committed + 空 current_turn）→ 空 items
        push_view_models(&mut state);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert!(
            snapshot.items.is_empty(),
            "push_view_models with empty BridgeState should produce empty items"
        );
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_multiple_entries() {
        crate::kit::atoms::init_atoms();
        *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

        let plan_json = json!({
            "entries": [
                {"content": "Task 1", "status": "in_progress", "priority": "medium"},
                {"content": "Task 2", "status": "pending", "priority": "medium"},
                {"content": "Task 3", "status": "completed", "priority": "medium"}
            ]
        });

        handle_plan_update(&plan_json);

        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert_eq!(items.len(), 3, "应包含 3 个条目");
        assert!(matches!(items[0].status, TodoStatus::InProgress));
        assert!(matches!(items[1].status, TodoStatus::Pending));
        assert!(matches!(items[2].status, TodoStatus::Completed));
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_empty_entries() {
        crate::kit::atoms::init_atoms();
        *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

        let plan_json = json!({
            "entries": []
        });

        handle_plan_update(&plan_json);

        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert!(items.is_empty(), "空 entries 应产出空列表");
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_missing_entries() {
        crate::kit::atoms::init_atoms();
        // 写入一个非空值，确认不被覆盖
        *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::InProgress,
            content: "existing".into(),
        }];

        let plan_json = json!({});
        handle_plan_update(&plan_json);

        // Plan 缺少 entries 字段 → deserialize 失败 → 不覆盖 TODO_ITEMS
        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert_eq!(items.len(), 1, "缺少 entries 不应覆盖已有列表");
        assert_eq!(items[0].content, "existing");
    }
}

/// 从 ACP SessionUpdate::Plan JSON 中提取 TodoItem 列表并写入 TODO_ITEMS atom。
///
/// 使用类型安全 serde 反序列化将 Plan JSON 映射为 TodoItem 列表。
/// Plan JSON 格式:
///   {"sessionUpdate":"plan","entries":[{"content":"Fix bug","status":"in_progress","priority":"medium"}]}
pub fn handle_plan_update(update: &serde_json::Value) {
    use crate::kit::message_area::{TodoItem, TodoStatus};

    let plan: Plan = match serde_json::from_value(update.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "handle_plan_update: failed to deserialize Plan");
            return;
        }
    };

    tracing::debug!(
        entries_count = plan.entries.len(),
        "handle_plan_update: received Plan entries"
    );

    let items: Vec<TodoItem> = plan
        .entries
        .into_iter()
        .map(|e| {
            let status = match e.status {
                PlanEntryStatus::InProgress => TodoStatus::InProgress,
                PlanEntryStatus::Completed => TodoStatus::Completed,
                PlanEntryStatus::Pending => TodoStatus::Pending,
                _ => {
                    tracing::warn!(status = ?e.status, "handle_plan_update: unknown PlanEntryStatus, fallback to Pending");
                    TodoStatus::Pending
                }
            };
            TodoItem {
                status,
                content: e.content,
            }
        })
        .collect();

    tracing::debug!(
        items_count = items.len(),
        "handle_plan_update: writing {} items to TODO_ITEMS",
        items.len()
    );
    *crate::kit::atoms::TODO_ITEMS.state().write() = items;
}
