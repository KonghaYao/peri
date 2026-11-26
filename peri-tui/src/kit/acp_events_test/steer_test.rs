use super::AcpEventData;
use super::BridgeState;
use super::SessionPhase;
use super::TuiRenderUnit;
use super::TuiTextChunk;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

use crate::kit::steer_state::{STEERS, SteerState};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::{UserInputQueueItem, UserInputQueueSnapshot, UserInputState};

struct RestoreSteers(SteerState);
impl Drop for RestoreSteers {
    fn drop(&mut self) {
        STEERS.set(self.0.clone());
    }
}

fn make_steer_bridge() -> (BridgeState, RestoreSteers) {
    let guard = RestoreSteers(STEERS.state().read().clone());
    STEERS.set(SteerState::default());
    let mut bridge = super::make_fold_test_state();
    bridge.active_session_id = "steer-session".into();
    let epoch = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let atom = STEERS.state();
    let mut steers = atom.write();
    steers.enabled = true;
    steers.reset_session(&bridge.active_session_id, epoch);
    steers.accept_snapshot(
        UserInputQueueSnapshot {
            session_id: bridge.active_session_id.clone(),
            generation: "g".into(),
            revision: 1,
            active_request_id: None,
            items: vec![UserInputQueueItem {
                input_id: "waiting".into(),
                content: MessageContent::text("等待"),
                original_draft: "等待".into(),
                plugin_selections: Vec::new(),
                state: UserInputState::Queued,
            }],
            terminal_items: vec![],
        },
        epoch,
        true,
    );
    (bridge, guard)
}

fn delivered() -> AcpEventData {
    AcpEventData::UserInputDelivered {
        generation: "g".into(),
        input_id: "accepted".into(),
        content: MessageContent::text("新的输入"),
    }
}

#[test]
#[serial]
fn input_delivery_without_a_current_run_does_not_start_loading() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(&mut state, &delivered());
    assert_eq!(state.committed.len(), 1);
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);
}

#[test]
#[serial]
fn input_delivery_preserves_loading_from_a_current_run_start() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("current-run".into()),
        },
    );
    super::dispatch_and_notify(&mut state, &delivered());
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert!(crate::kit::atoms::ACP_STATE.state().read().is_loading);
}

#[test]
#[serial]
fn test_steer_delivered_reuses_chat_bubble_between_assistant_turns() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "旧回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &delivered());
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "新回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(
        state.committed.len(),
        3,
        "新用户气泡应位于两段既有assistant渲染之间"
    );
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiAssistantBubble(_)),
        "先保留旧回答"
    );
    assert!(
        matches!(&state.committed[1], TuiRenderUnit::TuiUserBubble(_)),
        "仍使用原用户气泡"
    );
    assert!(
        matches!(&state.committed[2], TuiRenderUnit::TuiAssistantBubble(_)),
        "后续回答仍走原渲染"
    );
}

/// P0/M2 regression: a running background subagent must not hold the parent
/// turn open. A delivered prompt archives the preceding output in order, and
/// the following answer stays after the prompt.
#[test]
#[serial]
fn p0_delivered_prompt_follows_output_with_running_background_child() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "旧回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(&mut state, &delivered());

    let snapshot = VIEW_MODELS.state().read().clone();
    let items = &snapshot.items;
    let answer = items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiAssistantBubble(b) if b.text == "旧回答"))
        .expect("p0 delivered prompt follows output with running background child: fixture must contain the searched item");
    let group = items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiSubAgentGroup(_)))
        .expect("p0 delivered prompt follows output with running background child: fixture must contain the searched item");
    let prompt = items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiUserBubble(b) if b.text == "新的输入"))
        .expect("p0 delivered prompt follows output with running background child: fixture must contain the searched item");
    assert!(
        answer < group && group < prompt,
        "delivered prompt must follow existing output"
    );
    assert!(
        state.current_turn.is_empty(),
        "只有后台 child 运行时，delivered prompt 边界必须整段归档（M2）"
    );
    assert!(
        state.committed.iter().any(|vm| matches!(
            vm,
            TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "child"
        )),
        "运行中的后台分组随父消息一起归档，顺序不得丢失"
    );

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "新回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child".into(),
            result: String::new(),
            is_error: false,
        },
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let archived = VIEW_MODELS.state().read().clone();
    let prompt = archived
        .items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiUserBubble(b) if b.text == "新的输入"))
        .expect("p0 delivered prompt follows output with running background child: fixture must contain the searched item");
    let newer_answer = archived
        .items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiAssistantBubble(b) if b.text == "新回答"))
        .expect("p0 delivered prompt follows output with running background child: fixture must contain the searched item");
    assert!(
        prompt < newer_answer,
        "archived answer must remain after delivered prompt"
    );
}

/// P0 regression: canonical queue delivery must keep a running parent tool
/// available for the later ToolEnded event.
#[test]
#[serial]
fn p0_delivered_prompt_preserves_running_tool_result() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "tool-1".into(),
            tool_name: "Read".into(),
            input_summary: "file.rs".into(),
            raw_input: serde_json::json!({"path": "file.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &delivered());
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "tool-1".into(),
            output_summary: "file contents".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let snapshot = VIEW_MODELS.state().read().clone();
    let items = &snapshot.items;
    let tool = items.iter().position(|vm| {
        matches!(
            vm,
            TuiRenderUnit::TuiCollapsedGroup(group)
                if group.view_models.iter().any(|inner| matches!(inner, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-1" && card.output_summary == "file contents"))
        )
    });
    let prompt = items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiUserBubble(b) if b.text == "新的输入"));
    assert!(
        tool.is_some(),
        "delivered prompt must preserve the tool result"
    );
    assert!(
        tool < prompt,
        "delivered prompt must follow the previous tool"
    );
}

#[test]
#[serial]
fn test_steer_delivered_duplicate_only_adds_one_user_bubble() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(&mut state, &delivered());
    super::dispatch_and_notify(&mut state, &delivered());
    assert_eq!(
        state.committed.len(),
        1,
        "重复 canonical 事件不得产生重复气泡"
    );
}

#[test]
#[serial]
fn test_steer_replay_message_id_deduplicates_late_delivery() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReplayedUserBubble {
            input_id: "accepted".into(),
            text: "新的输入".into(),
        },
    );
    super::dispatch_and_notify(&mut state, &delivered());
    assert_eq!(
        state.committed.len(),
        1,
        "重放已有input ID后迟到Delivered不追加"
    );
}

#[test]
#[serial]
fn test_steer_stop_keeps_canonical_bubble_and_server_queue() {
    let (mut state, _restore) = make_steer_bridge();
    super::dispatch_and_notify(&mut state, &delivered());
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancelled".into(),
            request_id: None,
        },
    );
    assert_eq!(
        state.committed.len(),
        1,
        "已进入transcript的用户消息不可本地回滚"
    );
    assert_eq!(
        STEERS.state().read().rows(&state.active_session_id).len(),
        1,
        "停止不排空服务端待发投影"
    );
    assert!(
        !crate::kit::atoms::ACP_STATE.state().read().is_loading,
        "停止应结束loading"
    );
}

/// [回归测试] 空闲提交跳过待发送区，canonical Delivered 仍生成且仅生成一个气泡。
#[test]
#[serial]
fn test_steer_idle_input_goes_directly_to_chat_on_delivery() {
    use crate::kit::steer_state::{SteerCommand, SteerCommandKind};
    use peri_acp_types::session::UserInput;
    let (mut state, _restore) = make_steer_bridge();
    let epoch = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let mut snapshot = STEERS
        .state()
        .read()
        .snapshot(&state.active_session_id, epoch)
        .expect("steer idle input goes directly to chat on delivery: isolated fixture workspace must remain accessible")
        .clone();
    snapshot.revision += 1;
    snapshot.items.clear();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::UserInputQueueChanged { snapshot },
    );
    STEERS.state().write().begin(SteerCommand {
        session_id: state.active_session_id.clone(),
        epoch,
        command_id: "submit".into(),
        generation: Some("g".into()),
        auto_dispatch: false,
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: "accepted".into(),
            content: MessageContent::text("新的输入"),
            original_draft: "新的输入".into(),
            plugin_selections: Vec::new(),
        }),
    });
    assert!(
        STEERS
            .state()
            .read()
            .rows(&state.active_session_id)
            .is_empty(),
        "直接提交不显示队列行"
    );
    assert!(state.committed.is_empty(), "尚未确认不能伪造正式消息");
    super::dispatch_and_notify(&mut state, &delivered());
    super::dispatch_and_notify(&mut state, &delivered());
    assert_eq!(state.committed.len(), 1);
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "新的输入")
    );
    assert!(
        STEERS
            .state()
            .read()
            .rows(&state.active_session_id)
            .is_empty()
    );
}

use crate::kit::atoms::VIEW_MODELS;
