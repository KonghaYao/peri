//! 从 middleware_runner.rs 分离的测试模块
use super::*;
use crate::agent::stages::StageContext;
use crate::messages::{BaseMessage, MessageContent};
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
fn test_agent_context_add_message_dual_writes() {
    let ctx = make_context();
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("old")));

    let mut cx = make_context_from_stage(&ctx);
    cx.add_message(BaseMessage::human(MessageContent::text("new")));

    assert_eq!(cx.messages().len(), 2);
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 2);
}

#[test]
fn test_drain_recall_to_buffer() {
    let ctx = make_context();
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("recall-1".to_string());
        cx.push_recall("recall-2".to_string());
        let drained = cx.drain_recall();
        assert_eq!(drained.len(), 2);
        assert!(cx.drain_recall().is_empty());
        // 手动 drain 到 ctx.recall_buffer
        ctx.recall_buffer.write().extend(drained);
    }
    let recalls = ctx.recall_buffer.read();
    assert_eq!(recalls.len(), 2);
    assert_eq!(recalls[0], "recall-1");
    assert_eq!(recalls[1], "recall-2");
}

#[test]
fn test_recall_accumulates_across_hooks() {
    let ctx = make_context();
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("hook-1".to_string());
        let rec = cx.drain_recall();
        ctx.recall_buffer.write().extend(rec);
    }
    {
        let mut cx = make_context_from_stage(&ctx);
        cx.push_recall("hook-2".to_string());
        let rec = cx.drain_recall();
        ctx.recall_buffer.write().extend(rec);
    }
    let recalls = ctx.recall_buffer.read();
    assert_eq!(recalls.len(), 2);
    assert_eq!(recalls[0], "hook-1");
    assert_eq!(recalls[1], "hook-2");
}

#[test]
fn test_no_recall_keeps_buffer_empty() {
    let ctx = make_context();
    let mut cx = make_context_from_stage(&ctx);
    let drained = cx.drain_recall();
    assert!(drained.is_empty());
}
