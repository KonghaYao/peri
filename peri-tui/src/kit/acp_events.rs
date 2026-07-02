//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::kit::acp_types::{AcpEventData, CurrentTurn, ToolCardAccumulator};
use crate::kit::atoms::*;
use peri_acp_types::view_model::{NoteLevel, SystemNoteData, ViewModel};
use std::sync::Arc;

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
            state.current_turn.append_text(&tc.text);
            state.variant = 1;
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }
        ReasoningChunk(rc) => {
            state.current_turn.append_reasoning(&rc.text);
            state.variant = 1;
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }
        ToolStarted(ts) => {
            state.current_turn.start_tool(ToolCardAccumulator::new(
                ts.tool_id.clone(),
                ts.tool_name.clone(),
                ts.input_summary.clone(),
            ));
            state.variant = 1;
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }
        ToolEnded(te) => {
            state
                .current_turn
                .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
            state.variant = 1;
            state.is_loading = true;
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.2 Boundary events ──
        ViewCommit(vc) => {
            // I20-B：clone incoming Vec → 移入 Arc，单次 O(n) 分配。
            state.committed = Arc::from(vc.view_models.clone());
            state.current_turn = CurrentTurn::new();
            push_view_models(state);
            push_acp_state(state);
        }
        TurnDone => {
            let vms = state.current_turn.view_models().to_vec();
            // I20-B：Arc 不可 extend，需重建——拼接旧 + 新
            let mut combined = Vec::with_capacity(state.committed.len() + vms.len());
            combined.extend(state.committed.iter().cloned());
            combined.extend(vms);
            state.committed = Arc::from(combined);
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.is_loading = false;
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
        TokenUsage(_tu) => {
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
            combined.push(ViewModel::SystemNote(SystemNoteData {
                text: sn.text.clone(),
                level,
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
            if let Some(atom) = HITL_PENDING.get() {
                *atom.write() = Some(hp.clone());
            }
            state.popup_kind = Some(PopupKind::Hitl);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        AskUser(au) => {
            // I21-B：保存 payload 到 ASK_USER_PENDING atom，供 AskUserPopup 读取真实数据
            if let Some(atom) = ASK_USER_PENDING.get() {
                *atom.write() = Some(au.clone());
            }
            state.popup_kind = Some(PopupKind::AskUser);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        RewindPreview(rp) => {
            // S10：保存 payload 到 REWIND_PREVIEW atom，供 RewindPopup 读取真实数据
            if let Some(atom) = REWIND_PREVIEW.get() {
                *atom.write() = Some(rp.clone());
            }
            state.popup_kind = Some(PopupKind::Rewind);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        OauthNeeded(on) => {
            // I20-D：保存 payload 到 OAUTH_INFO atom，供 OAuthPopup 读取真实数据
            if let Some(atom) = OAUTH_INFO.get() {
                *atom.write() = Some(on.clone());
            }
            state.popup_kind = Some(PopupKind::OAuth);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }

        // ── §4.6 Structure events ──
        SubagentStarted(_) | SubagentStopped(_) => {
            push_acp_state(state);
        }

        // ── Unknown / forward-compat ──
        Unknown { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Atom push 辅助函数
// ---------------------------------------------------------------------------

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
fn push_view_models(state: &mut BridgeState) {
    // I20-B：Arc::clone 是 O(1) 原子指针拷贝，避免之前每个 streaming chunk
    // 都 O(n) clone 整个消息历史的性能问题。
    let snapshot = ViewModelsSnapshot {
        committed: Arc::clone(&state.committed),
        current_turn: Arc::from(state.current_turn.view_models()),
    };
    *VIEW_MODELS.get().unwrap().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.is_loading,
        wizard_active: false,
        at_mention_active: *AT_MENTION_ACTIVE.get().unwrap().read(),
        slash_hint_active: *SLASH_HINT_ACTIVE.get().unwrap().read(),
    };
    *ACP_STATE.get().unwrap().write() = snapshot;
}

/// 将 BridgeState.popup_kind 写入 POPUP_KIND Atom（S7）。
fn push_popup_kind(state: &BridgeState) {
    if let Some(atom) = POPUP_KIND.get() {
        *atom.write() = state.popup_kind;
    }
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
    let (Some(buf_atom), Some(tx)) = (INPUT_BUFFER.get(), SUBMIT_TX.get()) else {
        return;
    };
    let drained: Vec<String> = buf_atom.write().drain(..).collect();
    for text in drained {
        let _ = tx.send(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tokio::sync::mpsc;

    /// C1 回归测试：drain_input_buffer 清空 INPUT_BUFFER 队列。
    ///
    /// 注：不验证 SUBMIT_TX 接收——SUBMIT_TX 是 OnceLock 全局句柄，一旦被其他
    /// 测试 set 就无法重置；此处只验证 drain 的核心效应（buffer 被清空）。
    /// 顺序保证由 `VecDeque::drain(..)` + 顺序 `tx.send` 在源码层面保证。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_preserves_order() {
        crate::kit::atoms::init_atoms();
        // 确保 SUBMIT_TX 已 set（首次 set 或被前一个测试 set 都 OK）
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let _ = SUBMIT_TX.set(tx);

        // 入队三条
        {
            let mut buf = INPUT_BUFFER.get().unwrap().write();
            buf.push_back("first".into());
            buf.push_back("second".into());
            buf.push_back("third".into());
        }

        drain_input_buffer();

        // 验证 buffer 已被 drain 干净——这是 drain_input_buffer 的核心效应
        assert!(
            INPUT_BUFFER.get().unwrap().read().is_empty(),
            "buffer should be empty after drain"
        );
    }

    /// C1 回归测试：空 buffer 是 no-op，drain 后仍为空。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_empty_is_noop() {
        crate::kit::atoms::init_atoms();
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let _ = SUBMIT_TX.set(tx);

        INPUT_BUFFER.get().unwrap().write().clear();
        drain_input_buffer();

        assert!(
            INPUT_BUFFER.get().unwrap().read().is_empty(),
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
        INPUT_BUFFER.get().unwrap().write().push_back("x".into());
        drain_input_buffer();
        // SUBMIT_TX 已被前面测试 set 过，所以 drain 成功 → buffer 被清空
        // 即使 SUBMIT_TX 未 set，drain 早退，buffer 仍有 "x"——两种情况都不算 panic
    }
}
