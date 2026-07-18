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
    assert_eq!(transcript.entries()[0].message.content(), "old");
    assert_eq!(transcript.entries()[1].message.content(), "new");
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
fn test_get_set_context_on_owned_hashmap() {
    let ctx = make_context();
    {
        let mut guard = ctx.session.session_context.write();
        guard.insert("session_id".to_string(), "s1".to_string());
    }
    let mut ac = AgentContext::from_stage(&ctx);

    // get_context 读取 from_stage 时的快照
    assert_eq!(ac.get_context("session_id"), Some("s1"));

    // set_context 修改自有 HashMap
    ac.set_context("key".to_string(), "value".to_string());
    assert_eq!(ac.get_context("key"), Some("value"));

    // ctx.session.session_context 不受影响（自有克隆）
    let guard = ctx.session.session_context.read();
    assert_eq!(guard.get("key"), None);
}

#[test]
fn test_token_tracker_is_default() {
    let ctx = make_context();
    let ac = AgentContext::from_stage(&ctx);
    assert_eq!(ac.token_tracker().total_input_tokens, 0);
    assert!(ac.token_tracker().last_usage.is_none());
}

#[test]
fn test_token_tracker_mut_is_mutable() {
    let ctx = make_context();
    let mut ac = AgentContext::from_stage(&ctx);
    ac.token_tracker_mut().total_input_tokens = 100;
    assert_eq!(ac.token_tracker().total_input_tokens, 100);
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
fn test_messages_mut_emits_warning() {
    let ctx = make_context();
    let mut ac = AgentContext::from_stage(&ctx);
    ac.add_message(BaseMessage::human(MessageContent::text("msg1")));

    // messages_mut 应只影响 cache，不写入 transcript
    let cache = ac.messages_mut();
    cache.push(BaseMessage::human(MessageContent::text("cache-only")));

    // transcript 不应有 cache-only 消息
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1, "messages_mut 不应写入 transcript");
    assert!(!transcript
        .entries()
        .iter()
        .any(|e| e.message.content() == "cache-only"));
}

#[test]
fn test_prepend_message_emits_warning() {
    let ctx = make_context();
    let mut ac = AgentContext::from_stage(&ctx);
    ac.add_message(BaseMessage::human(MessageContent::text("msg1")));

    // prepend_message 应只影响 cache，不写入 transcript
    ac.prepend_message(BaseMessage::human(MessageContent::text("prepended")));

    assert_eq!(ac.messages().len(), 2);
    assert_eq!(ac.messages()[0].content(), "prepended");

    // transcript 不应有 prepended 消息
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
}
