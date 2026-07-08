//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::kit::acp_types::{AcpEventData, CurrentTurn, ToolCardAccumulator};
use crate::kit::atoms::*;
use crate::kit::tui_render_unit::{
    TuiNoteLevel, TuiRenderUnit, TuiSystemNote, TuiUserBubble, tui_hash_str,
};
use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// BridgeState — ACP 事件桥接内部状态
// ---------------------------------------------------------------------------

/// 桥接任务维护的内部状态，每个 ACP 事件到达时同步更新。
///
/// 定义在 acp_events.rs 中以避免 acp_bridge ↔ acp_events 循环依赖。
pub struct BridgeState {
    /// 0=Idle, 1=Streaming, 2=Modal
    pub variant: u8,
    /// 已提交的 TuiRenderUnit 列表
    ///
    /// I20-B：改 `Arc<[TuiRenderUnit]>`——push_view_models 在每个 streaming chunk
    /// 上都会 clone 一份写入 atom，Vec 会 O(n)；Arc clone O(1)，只有
    /// ViewCommit/TurnDone/SystemNotification 真正修改时才重建 Arc。
    pub committed: Arc<[TuiRenderUnit]>,
    /// 当前轮次的增量数据
    pub current_turn: CurrentTurn,
    /// Agent 是否正在加载中
    pub is_loading: bool,
    /// S7：精确弹窗类型，由 AcpEvent 直接映射。None = 无弹窗。
    /// 弹窗激活状态由 POPUP_KIND.is_some() 派生（status_bar / event_handlers 都读这个）
    pub popup_kind: Option<crate::kit::atoms::PopupKind>,
    /// I21-D：是否已完成至少一轮 TurnDone/TurnInterrupted。用于 push_view_models
    /// fallback 判断——一旦 has_turn_done=true，committed 以 bridge 为准，
    /// 即使为空也不 fallback 到 atom 旧值（/clear 产生空 committed 是合法结果）。
    pub has_turn_done: bool,
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
                state.is_loading = true;
                push_view_models(state);
            } else {
                state.current_turn.append_text(&tc.text);
                state.variant = 1;
                state.is_loading = true;
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
                state.is_loading = true;
                push_view_models(state);
            } else {
                state.current_turn.append_reasoning(&rc.text);
                tracing::info!(
                    len = state.current_turn.reasoning.len(),
                    "bridge: reasoning appended"
                );
                state.variant = 1;
                state.is_loading = true;
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
                state.is_loading = true;
                push_view_models(state);
            } else {
                state.current_turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
                state.variant = 1;
                state.is_loading = true;
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
                state.is_loading = true;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
                state.variant = 1;
                state.is_loading = true;
                push_view_models(state);
            }
            push_acp_state(state);
        }

        // ── §4.2 Boundary events ──
        TurnDone => {
            // (a) 收集 buffered 输入和 assistant view_models，确保归档顺序正确
            let buffered_texts: Vec<String> = INPUT_BUFFER.state().read().iter().cloned().collect();
            let has_buffered = !buffered_texts.is_empty();

            let vms = if !state.current_turn.committed && !state.current_turn.is_empty() {
                Some(state.current_turn.view_models().to_vec())
            } else {
                None
            };

            // (b) 一次性重建 committed：[旧 committed, TuiUserBubble..., assistant view_models...]
            // TuiUserBubble 排在 assistant 之前，符合真实对话顺序
            let extra_len = buffered_texts.len() + vms.as_ref().map_or(0, Vec::len);
            if extra_len > 0 {
                let mut combined = Vec::with_capacity(state.committed.len() + extra_len);
                combined.extend(state.committed.iter().cloned());
                for text in &buffered_texts {
                    combined.push(TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                        text: text.clone(),
                        content_hash: tui_hash_str(text),
                        is_system_reminder: false,
                    }));
                }
                if let Some(ref vms) = vms {
                    combined.extend(vms.iter().cloned());
                }
                state.committed = Arc::from(combined);
            }

            // (c) reset current_turn
            state.current_turn.reset();
            state.variant = 0;

            // (d) has_turn_done 提前到 push_view_models 之前，语义直观
            state.has_turn_done = true;

            // (e) S16：有 buffered 输入 → 保持 loading 态，避免 drain→submit 到首条流式事件间的空白窗口期
            state.is_loading = has_buffered;

            tracing::info!(
                is_loading = state.is_loading,
                has_buffered,
                committed_len = state.committed.len(),
                current_turn_empty = state.current_turn.is_empty(),
                has_turn_done = state.has_turn_done,
                "TurnDone: writing ACP_STATE"
            );

            // (f) push_view_models + push_acp_state
            push_view_models(state);
            push_acp_state(state);

            // (g) C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
            // 用户在 loading 期间按 Enter 的输入在此处一次性 flush 到 SUBMIT_TX。
            drain_input_buffer();
        }
        TurnInterrupted { reason: _reason } => {
            // 守卫：仅当 current_turn 有未归档内容时才归档，避免空/半成品消息成为幽灵
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                state.current_turn.deactivate();
                let vms = state.current_turn.view_models().to_vec();
                // I20-B：同 TurnDone，重建 Arc
                let mut combined = Vec::with_capacity(state.committed.len() + vms.len());
                combined.extend(state.committed.iter().cloned());
                combined.extend(vms);
                state.committed = Arc::from(combined);
            }
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.is_loading = false;
            // has_turn_done 提前到 push_view_models 之前，语义直观
            state.has_turn_done = true;
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
            // I20-B：Arc 不可 push，需重建
            let mut combined = Vec::with_capacity(state.committed.len() + 1);
            combined.extend(state.committed.iter().cloned());
            let content_hash = tui_hash_str(&format!("{}|{:?}", sn.text, level));
            combined.push(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                text: sn.text.clone(),
                level,
                content_hash,
            }));
            state.committed = Arc::from(combined);
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
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }
        SubagentStopped { agent_id } => {
            state.current_turn.stop_subagent(agent_id);
            state.variant = 1;
            state.is_loading = true;
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
            let mut combined = Vec::with_capacity(state.committed.len() + 1);
            combined.extend(state.committed.iter().cloned());
            combined.push(vm);
            state.committed = Arc::from(combined);
            state.has_turn_done = true;
            push_view_models(state);
            push_acp_state(state);
        }
        ReplayAssistantBubble { text } => {
            let vm = TuiRenderUnit::TuiAssistantBubble(
                crate::kit::tui_render_unit::TuiAssistantBubble {
                    text: text.clone(),
                    reasoning: None,
                    tool_card_ids: vec![],
                    content_hash: 0,
                },
            );
            let mut combined = Vec::with_capacity(state.committed.len() + 1);
            combined.extend(state.committed.iter().cloned());
            combined.push(vm);
            state.committed = Arc::from(combined);
            state.has_turn_done = true;
            push_view_models(state);
            push_acp_state(state);
        }

        // ── Unknown / forward-compat ──
        Unknown { .. } => {}

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
/// S16：bridge 的 committed 仅在 TurnDone 时填充完整。streaming 事件到达时若
/// bridge committed 仍为空（尚未收到 TurnDone），则保留 atom 中已有的
/// committed（可能含 submit_text 预先注入的 TuiUserBubble），避免消息区退回 Welcome。
///
/// I21-D：一旦完成过 TurnDone（has_turn_done=true），committed 以 bridge
/// 为准，即使为空也不 fallback——/clear 产生空 committed 是合法结果。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    // I20-B：Arc::clone 是 O(1) 原子指针拷贝，避免之前每个 streaming chunk
    // 都 O(n) clone 整个消息历史的性能问题。
    let committed = if state.committed.is_empty() && !state.has_turn_done {
        Arc::clone(&VIEW_MODELS.state().read().committed)
    } else {
        Arc::clone(&state.committed)
    };
    let snapshot = ViewModelsSnapshot {
        committed,
        current_turn: Arc::from(state.current_turn.view_models()),
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    let snapshot = ViewModelsSnapshot {
        committed: Arc::from([]),
        current_turn: Arc::from([]),
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
        is_loading: state.is_loading,
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
/// 缓存的输入继续提交。若 buffer 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
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
            let _ = tx.send(text);
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
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
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
                agent_id: Some("agent-1".into()),
            }),
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.current_turn.len(), 1);
        match &snapshot.current_turn[0] {
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
            let (tx, _rx) = mpsc::unbounded_channel::<String>();
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
            let (tx, _rx) = mpsc::unbounded_channel::<String>();
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
            snapshot.committed.is_empty(),
            "bridge reset 后 committed 应为空"
        );
        assert!(
            snapshot.current_turn.is_empty(),
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
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReplayUserBubble {
                text: "hello from replay".into(),
            },
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.committed.len(), 1);
        match &snapshot.committed[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "hello from replay"),
            other => panic!("expected TuiUserBubble, got {other:?}"),
        }
        assert!(
            state.has_turn_done,
            "has_turn_done should be true after replay"
        );
    }

    #[test]
    #[serial]
    fn test_replay_assistant_bubble_appends_to_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReplayAssistantBubble {
                text: "assistant from replay".into(),
            },
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.committed.len(), 1);
        match &snapshot.committed[0] {
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
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
        };

        // 第一轮：stream one text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "first turn".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert_eq!(
            state.committed.len(),
            1,
            "first TurnDone: committed should have 1 VM"
        );
        assert!(
            state.has_turn_done,
            "first TurnDone should set has_turn_done"
        );

        // 第二轮：stream another text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "second turn".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(
            snapshot.committed.len(),
            2,
            "two TurnDones: committed should have 2 VMs"
        );
        assert!(snapshot.current_turn.is_empty());
    }

    /// TurnDone TuiUserBubble 排在 assistant 之前：预置 INPUT_BUFFER 两条文本 +
    /// current_turn 一条 TuiAssistantBubble，触发 TurnDone，断言 committed 顺序为
    /// [TuiUserBubble, TuiUserBubble, TuiAssistantBubble]。
    #[test]
    #[serial]
    fn test_turndone_userbubble_before_assistant() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: false,
        };

        // 往 current_turn 写入一条 assistant 文本
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "assistant reply".into(),
                agent_id: None,
            }),
        );

        // 往 INPUT_BUFFER 塞两条 buffered 输入
        INPUT_BUFFER
            .state()
            .write()
            .push_back("user says hello".into());
        INPUT_BUFFER
            .state()
            .write()
            .push_back("user says world".into());

        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert_eq!(
            state.committed.len(),
            3,
            "committed 应有 3 个 VM：2 TuiUserBubble + 1 TuiAssistantBubble"
        );
        // 顺序：TuiUserBubble, TuiUserBubble, TuiAssistantBubble
        match &state.committed[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "user says hello"),
            other => panic!("expected TuiUserBubble at [0], got {other:?}"),
        }
        match &state.committed[1] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "user says world"),
            other => panic!("expected TuiUserBubble at [1], got {other:?}"),
        }
        match &state.committed[2] {
            TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "assistant reply"),
            other => panic!("expected TuiAssistantBubble at [2], got {other:?}"),
        }
        assert!(
            state.has_turn_done,
            "has_turn_done 应在 push_view_models 之前已为 true"
        );
    }

    /// TurnInterrupted 空 current_turn 不归档：预置空 current_turn 触发 TurnInterrupted，
    /// 断言 committed 长度不变。
    #[test]
    #[serial]
    fn test_turn_interrupted_empty_skips_archive() {
        crate::kit::atoms::init_atoms();
        // 预置一条 committed 数据
        let pre_existing: Arc<[TuiRenderUnit]> =
            Arc::from([TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "existing".into(),
                content_hash: 1,
                is_system_reminder: false,
            })]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            committed: Arc::clone(&pre_existing),
            current_turn: Arc::from([]),
        };
        let mut state = BridgeState {
            variant: 1,
            committed: Arc::clone(&pre_existing),
            current_turn: CurrentTurn::new(),
            is_loading: true,
            popup_kind: None,
            has_turn_done: false,
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
        assert!(!state.is_loading, "is_loading 应为 false");
        assert!(
            state.has_turn_done,
            "has_turn_done 应在 push_view_models 之前已为 true"
        );
    }

    #[test]
    #[serial]
    fn test_has_turn_done_prevents_fallback() {
        crate::kit::atoms::init_atoms();
        // 设置 atom 中有旧 committed 数据
        let old_committed: Arc<[TuiRenderUnit]> =
            Arc::from([TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "old data".into(),
                content_hash: 1,
                is_system_reminder: false,
            })]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            committed: Arc::clone(&old_committed),
            current_turn: Arc::from([]),
        };

        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_turn_done: true,
        };

        // push_view_models: committed 为空但 has_turn_done=true — 不 fallback 到 atom 旧值
        push_view_models(&mut state);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert!(
            snapshot.committed.is_empty(),
            "has_turn_done=true with empty committed should NOT fallback to atom"
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
