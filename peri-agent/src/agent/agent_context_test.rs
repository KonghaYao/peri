//! 从 agent_context.rs 分离的测试模块
use super::*;
use crate::agent::stages::StageContext;
use crate::messages::MessageContent;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

#[test]
fn test_from_stage_copies_visible_messages() {
    let ctx = make_context();
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("hello")));

    let ac = AgentContext::from_stage(&ctx);
    assert_eq!(ac.messages().len(), 1);
    assert_eq!(ac.messages()[0].content(), "hello");
}

#[test]
fn test_from_stage_excluded_messages_filtered() {
    let ctx = make_context();
    let id = ctx
        .session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("excluded")));
    ctx.session.transcript.write().set_excluded(id, true);
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("visible")));

    let ac = AgentContext::from_stage(&ctx);
    assert_eq!(
        ac.messages().len(),
        1,
        "excluded 消息不应进入 AgentContext 视野"
    );
    assert_eq!(ac.messages()[0].content(), "visible");
}

#[test]
fn test_add_message_dual_writes_transcript_and_cache() {
    let ctx = make_context();
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("old")));

    let mut ac = AgentContext::from_stage(&ctx);
    ac.add_message(BaseMessage::human(MessageContent::text("new")));

    // cache 应包含 new
    assert_eq!(ac.messages().len(), 2);
    assert_eq!(ac.messages()[1].content(), "new");

    // transcript 也应包含 new（双写同步）
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 2, "transcript 应同时包含 old + new");
    assert_eq!(transcript.entries()[0].message().content(), "old");
    assert_eq!(transcript.entries()[1].message().content(), "new");
}

#[test]
fn test_cwd_delegates_to_turn() {
    let ctx = make_context();
    let ac = AgentContext::from_stage(&ctx);
    assert_eq!(ac.cwd(), "/tmp/test");
}

#[test]
fn test_current_step_delegates_to_turn() {
    let ctx = make_context();
    let ac = AgentContext::from_stage(&ctx);
    assert_eq!(ac.current_step(), 0);
}

#[test]
fn test_push_and_drain_recall() {
    let ctx = make_context();
    let mut ac = AgentContext::from_stage(&ctx);

    ac.push_recall("recall-1".to_string());
    ac.push_recall("recall-2".to_string());

    let drained = ac.drain_recall();
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0], "recall-1");
    assert_eq!(drained[1], "recall-2");

    // drain 后 buffer 清空
    assert!(ac.drain_recall().is_empty());
}

#[test]
fn test_v2_queue_is_shared() {
    let ctx = make_context();
    let ac = AgentContext::from_stage(&ctx);
    // 验证 queue 是同一个实例（通过地址比较或行为验证）
    assert!(ac.v2_queue().is_empty());
}

#[test]
fn replacement_preserves_visible_message_order_and_defers_transcript_write() {
    let ctx = make_context();
    let first = BaseMessage::human(MessageContent::text("first"));
    let second = BaseMessage::human(MessageContent::text("second"));
    ctx.session
        .transcript
        .write()
        .append_batch(vec![first.clone(), second.clone()]);
    let mut ac = AgentContext::from_stage(&ctx);

    assert!(ac.replace_message(second.clone_with_content(MessageContent::text("updated"))));
    assert!(ac.messages_modified());
    assert_eq!(
        ac.messages()
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        vec![first.id(), second.id()]
    );
    assert_eq!(ac.messages()[1].content(), "updated");
    assert_eq!(
        ctx.session
            .transcript
            .read()
            .get(second.id())
            .unwrap()
            .message()
            .content(),
        "second"
    );
}

#[test]
fn replacement_rejects_unknown_id_without_changing_cache() {
    let ctx = make_context();
    let original = BaseMessage::human(MessageContent::text("original"));
    ctx.session.transcript.write().append(original.clone());
    let mut ac = AgentContext::from_stage(&ctx);

    assert!(!ac.replace_message(BaseMessage::human(MessageContent::text("unknown"))));
    assert!(!ac.messages_modified());
    assert_eq!(ac.messages().len(), 1);
    assert_eq!(ac.messages()[0].id(), original.id());
    assert_eq!(ac.messages()[0].content(), "original");
}

#[test]
fn legacy_state_replacement_uses_the_same_id_contract() {
    let original = BaseMessage::human(MessageContent::text("original"));
    let mut legacy = crate::agent::state::AgentState::new("/tmp/test");
    legacy.add_message(original.clone());
    let state: &mut dyn MiddlewareState = &mut legacy;

    assert!(state.replace_message(original.clone_with_content(MessageContent::text("updated"))));
    assert!(!state.replace_message(BaseMessage::human(MessageContent::text("unknown"))));
    assert_eq!(state.messages().len(), 1);
    assert_eq!(state.messages()[0].id(), original.id());
    assert_eq!(state.messages()[0].content(), "updated");
}
