//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::kit::atoms::*;
use crate::state_machine::current_turn::{CurrentTurn, ToolCardAccumulator};
use crate::state_machine::event::AcpEventData;
use peri_acp_types::view_model::{NoteLevel, SystemNoteData, ViewModel};

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
    pub committed: Vec<ViewModel>,
    /// 当前轮次的增量数据
    pub current_turn: CurrentTurn,
    /// Agent 是否正在加载中
    pub is_loading: bool,
    /// 是否有交互弹窗挂起（仅作为状态栏的"是否有弹窗"指示，精确路由用 popup_kind）
    pub popup_active: bool,
    /// S7：精确弹窗类型，由 AcpEvent 直接映射。None = 无弹窗。
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
            state.committed = vc.view_models.clone();
            state.current_turn = CurrentTurn::new();
            push_view_models(state);
            push_acp_state(state);
        }
        TurnDone => {
            let vms = state.current_turn.view_models().to_vec();
            state.committed.extend(vms);
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.is_loading = false;
            push_view_models(state);
            push_acp_state(state);
        }
        TurnInterrupted(_ti) => {
            state.current_turn.deactivate();
            let vms = state.current_turn.view_models().to_vec();
            state.committed.extend(vms);
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
            state.committed.push(ViewModel::SystemNote(SystemNoteData {
                text: sn.text.clone(),
                level,
            }));
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.4 Input assist (no-op for now) ──
        Prediction(_) | FileSuggestions(_) => {}

        // ── §4.5 Interaction events ──
        // S7：把每个交互事件映射到具体 PopupKind，让 PopupOverlay 精确路由
        HitlPending(_) => {
            state.popup_kind = Some(PopupKind::Hitl);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        AskUser(_) => {
            state.popup_kind = Some(PopupKind::AskUser);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        RewindPreview(_) => {
            state.popup_kind = Some(PopupKind::Rewind);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        OauthNeeded(_) => {
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
    let snapshot = ViewModelsSnapshot {
        committed: state.committed.clone(),
        current_turn: state.current_turn.view_models().to_vec(),
    };
    *VIEW_MODELS.get().unwrap().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.is_loading,
        popup_active: state.popup_active,
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
