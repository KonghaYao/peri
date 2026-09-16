use super::*;
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
    let mut bridge = make_fold_test_state();
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
                state: UserInputState::Queued,
            }],
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
fn test_steer_delivered_reuses_chat_bubble_between_assistant_turns() {
    let (mut state, _restore) = make_steer_bridge();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "旧回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &delivered());
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "新回答".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
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

#[test]
#[serial]
fn test_steer_delivered_duplicate_only_adds_one_user_bubble() {
    let (mut state, _restore) = make_steer_bridge();
    dispatch_and_notify(&mut state, &delivered());
    dispatch_and_notify(&mut state, &delivered());
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
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReplayedUserBubble {
            input_id: "accepted".into(),
            text: "新的输入".into(),
        },
    );
    dispatch_and_notify(&mut state, &delivered());
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
    dispatch_and_notify(&mut state, &delivered());
    dispatch_and_notify(
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
        .unwrap()
        .clone();
    snapshot.revision += 1;
    snapshot.items.clear();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::UserInputQueueChanged { snapshot },
    );
    STEERS.state().write().begin(SteerCommand {
        session_id: state.active_session_id.clone(),
        epoch,
        command_id: "submit".into(),
        generation: Some("g".into()),
        kind: SteerCommandKind::Enqueue(UserInput {
            input_id: "accepted".into(),
            content: MessageContent::text("新的输入"),
            original_draft: "新的输入".into(),
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
    dispatch_and_notify(&mut state, &delivered());
    dispatch_and_notify(&mut state, &delivered());
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
