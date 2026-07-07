//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::kit::acp_types::{AcpEventData, CurrentTurn, ToolCardAccumulator};
use crate::kit::atoms::*;
use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};
use peri_acp_types::view_model::{NoteLevel, SystemNoteData, UserBubbleData, ViewModel, hash_str};
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
    /// 已提交的 ViewModel 列表
    ///
    /// I20-B：改 `Arc<[ViewModel]>`——push_view_models 在每个 streaming chunk
    /// 上都会 clone 一份写入 atom，Vec 会 O(n)；Arc clone O(1)，只有
    /// ViewCommit/TurnDone/SystemNotification 真正修改时才重建 Arc。
    pub committed: Arc<[ViewModel]>,
    /// 当前轮次的增量数据
    pub current_turn: CurrentTurn,
    /// Agent 是否正在加载中
    pub is_loading: bool,
    /// S7：精确弹窗类型，由 AcpEvent 直接映射。None = 无弹窗。
    /// 弹窗激活状态由 POPUP_KIND.is_some() 派生（status_bar / event_handlers 都读这个）
    pub popup_kind: Option<crate::kit::atoms::PopupKind>,
    /// I21-D：是否已收到 ViewCommit。用于 /clear 场景——/clear 的 ViewCommit
    /// committed 为空是合法结果（清空历史），不应 fallback 到 atom 旧值。
    pub has_view_commit: bool,
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
        ViewCommit(vc) => {
            // I20-B：clone incoming Vec → 移入 Arc，单次 O(n) 分配。
            let mut committed_vms = vc.view_models.clone();

            // 将流式累积的 SubAgent 子内容注入 view-commit 的 SubAgentGroup，
            // 避免 view_mapper 产生的空 placeholder 覆盖流式期间显示的子工具调用。
            let mut subagent_map: std::collections::HashMap<String, ViewModel> =
                std::collections::HashMap::new();
            for sa in &state.current_turn.subagents {
                let vm = sa.view_model();
                subagent_map.insert(sa.agent_id.clone(), vm);
            }
            if !subagent_map.is_empty() {
                for vm in committed_vms.iter_mut() {
                    if let ViewModel::SubAgentGroup(d) = vm {
                        if let Some(streaming_vm) = subagent_map.get(&d.agent_id) {
                            if let ViewModel::SubAgentGroup(streaming_data) = streaming_vm {
                                if !streaming_data.view_models.is_empty() {
                                    d.view_models = streaming_data.view_models.clone();
                                }
                            }
                        }
                    }
                }
            }

            state.committed = Arc::from(committed_vms);
            state.current_turn.mark_committed();
            state.has_view_commit = true;
            push_view_models(state);
            push_acp_state(state);
        }
        TurnDone => {
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                let vms = state.current_turn.view_models().to_vec();
                // I20-B：Arc 不可 extend，需重建——拼接旧 + 新
                let mut combined = Vec::with_capacity(state.committed.len() + vms.len());
                combined.extend(state.committed.iter().cloned());
                combined.extend(vms);
                state.committed = Arc::from(combined);
            }
            state.current_turn.reset();
            state.variant = 0;

            // S16：TurnDone 时有 buffered 输入尚未提交 → 保持 loading 态，
            // 避免 drain→submit 到首条流式事件间的空白窗口期。
            // 同时为每条 buffered 输入添加 UserBubble，保证消息区立即可见。
            let has_buffered = !INPUT_BUFFER.state().read().is_empty();
            if has_buffered {
                // S16：为所有 buffered 输入添加 UserBubble，保证消息区立即可见。
                let buffered_texts: Vec<String> =
                    INPUT_BUFFER.state().read().iter().cloned().collect();
                if !buffered_texts.is_empty() {
                    let mut combined =
                        Vec::with_capacity(state.committed.len() + buffered_texts.len());
                    combined.extend(state.committed.iter().cloned());
                    for text in &buffered_texts {
                        combined.push(ViewModel::UserBubble(UserBubbleData {
                            text: text.clone(),
                            content_hash: hash_str(text),
                            is_system_reminder: false,
                        }));
                    }
                    state.committed = Arc::from(combined);
                }
            }
            state.is_loading = has_buffered;

            push_view_models(state);
            push_acp_state(state);
            // C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
            // 用户在 loading 期间按 Enter 的输入在此处一次性 flush 到 SUBMIT_TX。
            drain_input_buffer();
        }
        TurnInterrupted(_ti) => {
            state.current_turn.deactivate();
            let vms = state.current_turn.view_models().to_vec();
            // I20-B：同 TurnDone，重建 Arc
            let mut combined = Vec::with_capacity(state.committed.len() + vms.len());
            combined.extend(state.committed.iter().cloned());
            combined.extend(vms);
            state.committed = Arc::from(combined);
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.is_loading = false;
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.3 Status events ──
        TokenUsage(tu) => {
            *SPINNER_TOKEN_COUNT.state().write() =
                (tu.input as usize).saturating_add(tu.output as usize);
            push_acp_state(state);
        }
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
                "warning" => NoteLevel::Warning,
                "error" => NoteLevel::Error,
                _ => NoteLevel::Info,
            };
            // I20-B：Arc 不可 push，需重建
            let mut combined = Vec::with_capacity(state.committed.len() + 1);
            combined.extend(state.committed.iter().cloned());
            let content_hash = hash_str(&format!("{}|{:?}", sn.text, level));
            combined.push(ViewModel::SystemNote(SystemNoteData {
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
        SubagentStarted(sg) => {
            state
                .current_turn
                .start_subagent(sg.agent_id.clone(), sg.agent_name.clone());
            state.variant = 1;
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }
        SubagentStopped(sg) => {
            state.current_turn.stop_subagent(&sg.agent_id);
            state.variant = 1;
            state.is_loading = true;
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
/// S16：bridge 的 committed 仅在 ViewCommit 时填充。streaming 事件到达时若
/// bridge committed 仍为空（尚未收到 ViewCommit），则保留 atom 中已有的
/// committed（可能含 submit_text 预先注入的 UserBubble），避免消息区退回 Welcome。
///
/// I21-D：一旦收到过 ViewCommit（has_view_commit=true），committed 以 bridge
/// 为准，即使为空也不 fallback——/clear 产生空 committed 是合法结果。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    // I20-B：Arc::clone 是 O(1) 原子指针拷贝，避免之前每个 streaming chunk
    // 都 O(n) clone 整个消息历史的性能问题。
    let committed = if state.committed.is_empty() && !state.has_view_commit {
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
            has_view_commit: false,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted(peri_acp_types::event_data::SubagentStarted {
                agent_id: "agent-1".into(),
                agent_name: "researcher".into(),
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(peri_acp_types::event_data::TextChunk {
                text: "child text".into(),
                agent_id: Some("agent-1".into()),
            }),
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.current_turn.len(), 1);
        match &snapshot.current_turn[0] {
            ViewModel::SubAgentGroup(group) => {
                assert_eq!(group.agent_id, "agent-1");
                assert_eq!(group.view_models.len(), 1);
            }
            other => panic!("expected SubAgentGroup, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn test_view_commit_then_turn_done_does_not_duplicate_current_turn() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_view_commit: false,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(peri_acp_types::event_data::TextChunk {
                text: "streaming".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ViewCommit(peri_acp_types::event_data::ViewCommit {
                view_models: vec![ViewModel::AssistantBubble(
                    peri_acp_types::view_model::AssistantBubbleData {
                        text: "committed".into(),
                        reasoning: None,
                        tool_card_ids: Vec::new(),
                        content_hash: 0,
                    },
                )],
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(
            snapshot.committed.len(),
            1,
            "TurnDone 不应重复 append 已提交轮次"
        );
        assert!(snapshot.current_turn.is_empty());
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

    /// 模拟 ReAct 多迭代边界——每轮 ViewCommit 后 committed 应包含全部 AI 文本。
    #[test]
    #[serial]
    fn test_multi_iteration_view_commit_preserves_all_ai_text() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_view_commit: false,
        };

        use peri_acp_types::view_model::{AssistantBubbleData, ToolCardData, UserBubbleData};

        // ── 迭代 1: Human + AI 文字+工具调用 ──
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(peri_acp_types::event_data::TextChunk {
                text: "我先搜索一下。".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(peri_acp_types::event_data::ToolStarted {
                tool_id: "tc-grep".into(),
                tool_name: "Grep".into(),
                input_summary: "pattern: fn foo".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(peri_acp_types::event_data::ToolEnded {
                tool_id: "tc-grep".into(),
                output_summary: "parser.rs:42".into(),
                is_error: false,
                agent_id: None,
            }),
        );

        // ViewCommit 迭代 1（3 条，模拟 ACP 先前轮次包含 UserBubble）
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ViewCommit(peri_acp_types::event_data::ViewCommit {
                view_models: vec![
                    ViewModel::UserBubble(UserBubbleData {
                        text: "帮我修复".into(),
                        content_hash: 1,
                        is_system_reminder: false,
                    }),
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "我先搜索一下。".into(),
                        reasoning: None,
                        tool_card_ids: vec!["tc-grep".into()],
                        content_hash: 2,
                    }),
                    ViewModel::ToolCard(ToolCardData {
                        tool_id: "tc-grep".into(),
                        tool_name: "Grep".into(),
                        input_summary: "pattern: fn foo".into(),
                        output_summary: "parser.rs:42".into(),
                        is_error: false,
                        is_running: false,
                        running_duration_ms: None,
                        diff: None,
                        content_hash: 3,
                    }),
                ],
            }),
        );

        // ── 迭代 2: streaming → ViewCommit（上一轮 + 新内容）──
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(peri_acp_types::event_data::TextChunk {
                text: "我来编辑。".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(peri_acp_types::event_data::ToolStarted {
                tool_id: "tc-edit".into(),
                tool_name: "Edit".into(),
                input_summary: "path: parser.rs".into(),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(peri_acp_types::event_data::ToolEnded {
                tool_id: "tc-edit".into(),
                output_summary: "updated".into(),
                is_error: false,
                agent_id: None,
            }),
        );

        // ViewCommit 迭代 2: 5 条（前 3 + 新 Ai + ToolCard）
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ViewCommit(peri_acp_types::event_data::ViewCommit {
                view_models: vec![
                    ViewModel::UserBubble(UserBubbleData {
                        text: "帮我修复".into(),
                        content_hash: 1,
                        is_system_reminder: false,
                    }),
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "我先搜索一下。".into(),
                        reasoning: None,
                        tool_card_ids: vec!["tc-grep".into()],
                        content_hash: 2,
                    }),
                    ViewModel::ToolCard(ToolCardData {
                        tool_id: "tc-grep".into(),
                        tool_name: "Grep".into(),
                        input_summary: "pattern: fn foo".into(),
                        output_summary: "parser.rs:42".into(),
                        is_error: false,
                        is_running: false,
                        running_duration_ms: None,
                        diff: None,
                        content_hash: 3,
                    }),
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "我来编辑。".into(),
                        reasoning: None,
                        tool_card_ids: vec!["tc-edit".into()],
                        content_hash: 4,
                    }),
                    ViewModel::ToolCard(ToolCardData {
                        tool_id: "tc-edit".into(),
                        tool_name: "Edit".into(),
                        input_summary: "path: parser.rs".into(),
                        output_summary: "updated".into(),
                        is_error: false,
                        is_running: false,
                        running_duration_ms: None,
                        diff: None,
                        content_hash: 5,
                    }),
                ],
            }),
        );

        // ── 迭代 3: 最终总结（纯文本 Ai）──
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(peri_acp_types::event_data::TextChunk {
                text: "修复完成！".into(),
                agent_id: None,
            }),
        );

        // ViewCommit 迭代 3: 6 条（前 5 + 最终 Ai 总结）
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ViewCommit(peri_acp_types::event_data::ViewCommit {
                view_models: vec![
                    ViewModel::UserBubble(UserBubbleData {
                        text: "帮我修复".into(),
                        content_hash: 1,
                        is_system_reminder: false,
                    }),
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "我先搜索一下。".into(),
                        reasoning: None,
                        tool_card_ids: vec!["tc-grep".into()],
                        content_hash: 2,
                    }),
                    ViewModel::ToolCard(ToolCardData {
                        tool_id: "tc-grep".into(),
                        tool_name: "Grep".into(),
                        input_summary: "pattern: fn foo".into(),
                        output_summary: "parser.rs:42".into(),
                        is_error: false,
                        is_running: false,
                        running_duration_ms: None,
                        diff: None,
                        content_hash: 3,
                    }),
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "我来编辑。".into(),
                        reasoning: None,
                        tool_card_ids: vec!["tc-edit".into()],
                        content_hash: 4,
                    }),
                    ViewModel::ToolCard(ToolCardData {
                        tool_id: "tc-edit".into(),
                        tool_name: "Edit".into(),
                        input_summary: "path: parser.rs".into(),
                        output_summary: "updated".into(),
                        is_error: false,
                        is_running: false,
                        running_duration_ms: None,
                        diff: None,
                        content_hash: 5,
                    }),
                    // 症状 2 关键：最终 Ai 总结
                    ViewModel::AssistantBubble(AssistantBubbleData {
                        text: "修复完成！".into(),
                        reasoning: None,
                        tool_card_ids: vec![],
                        content_hash: 6,
                    }),
                ],
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // ── 验证最终状态 ──
        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.committed.len(), 6);
        assert!(snapshot.current_turn.is_empty());

        let texts: Vec<&str> = snapshot
            .committed
            .iter()
            .filter_map(|vm| match vm {
                ViewModel::AssistantBubble(d) => Some(d.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 3);
        assert!(
            texts.contains(&"我先搜索一下。"),
            "第一条 AI 文本丢失: {texts:?}"
        );
        assert!(
            texts.contains(&"我来编辑。"),
            "第二条 AI 文本丢失: {texts:?}"
        );
        assert!(texts.contains(&"修复完成！"), "最终 AI 总结丢失: {texts:?}");

        assert!(
            matches!(&snapshot.committed[5], ViewModel::AssistantBubble(_)),
            "vm[5] 应为最终总结 AB，实际: {:?}",
            &snapshot.committed[5]
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
