//! Tests for acp_events

use super::*;
use crate::kit::acp_types::{
    AcpEventWithEpoch, FeedbackChannel, FeedbackLevel, TuiCommandFeedback,
};
use crate::kit::message_area::TodoStatus;
use crate::kit::tui_render_unit::{
    InteractionKind, TuiAskUserBlock, TuiTodoChangeKind, TuiToolPresentation,
};
use peri_acp_types::event_data::{AskUser, HitlPending, Question, QuestionOption};
use serde_json::json;
use serial_test::serial;
use tokio::sync::mpsc;

use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{EntryStatus, FoldKey, FoldState, TuiReasoningBlock};

// 本文件经 acp_events/mod.rs 的 `#[path = "../acp_events_test.rs"]` 挂载；此路径
// 加载方式下，rustc 不会为聚合根派生 `acp_events_test/` 子目录，子模块需显式
// `#[path]` 指向。
#[path = "acp_events_test/command_feedback_test.rs"]
mod command_feedback_test;
#[path = "acp_events_test/diff_grouping_test.rs"]
mod diff_grouping_test;
#[path = "acp_events_test/fold_test.rs"]
mod fold_test;
#[path = "acp_events_test/input_buffer_test.rs"]
mod input_buffer_test;
#[path = "acp_events_test/interaction_test.rs"]
mod interaction_test;
#[path = "acp_events_test/session_events_test.rs"]
mod session_events_test;
#[path = "acp_events_test/snapshot_test.rs"]
mod snapshot_test;
#[path = "acp_events_test/streaming_test.rs"]
mod streaming_test;
#[path = "acp_events_test/subagent_loading_test.rs"]
mod subagent_loading_test;
#[path = "acp_events_test/todo_skill_test.rs"]
mod todo_skill_test;
#[path = "acp_events_test/turn_archive_test.rs"]
mod turn_archive_test;
#[path = "acp_events_test/turn_interrupted_test.rs"]
mod turn_interrupted_test;

/// 确保全局 SUBMIT_TX 已初始化，并尽可能安装可观察的 receiver。
///
/// 返回 Some(rx)：本次测试**新安装**了 channel，可断言 drain 实际发出的
/// SubmitRequest 消息；返回 None：channel 已由先前测试安装（OnceLock 全局
/// 单例无法重置，只能断言 drain 的核心效应——buffer 被清空）。
fn ensure_submit_tx_observable() -> Option<mpsc::UnboundedReceiver<SubmitRequest>> {
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    SUBMIT_TX.set(tx).ok()?;
    Some(rx)
}

fn make_fold_test_state() -> BridgeState {
    crate::kit::atoms::init_atoms();
    *FOLD_OVERRIDES.state().write() = std::collections::HashMap::new();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    BridgeState {
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
    }
}

fn reasoning_of(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiReasoningBlock {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAssistantBubble(b) => b.reasoning.as_ref().expect("应含 reasoning"),
        other => panic!("expected TuiAssistantBubble at [{idx}], got {other:?}"),
    }
}
