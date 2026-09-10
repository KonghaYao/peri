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

struct ReplaceAppendRecall {
    fail: bool,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for ReplaceAppendRecall {
    fn name(&self) -> &str {
        "ReplaceAppendRecall"
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> crate::error::AgentResult<()> {
        let original = state
            .messages()
            .iter()
            .find(|message| message.content() == "original")
            .unwrap()
            .clone();
        assert!(state.replace_message(original.clone_with_content(MessageContent::text("updated"))));
        state.add_message(BaseMessage::human(MessageContent::text("added")));
        state.push_recall("replacement recall".to_string());
        if self.fail {
            return Err(crate::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: "failure after replacement".to_string(),
            });
        }
        Ok(())
    }
}

struct ObserveReplacement(Arc<std::sync::atomic::AtomicBool>);

#[async_trait::async_trait]
impl crate::middleware::Middleware for ObserveReplacement {
    fn name(&self) -> &str {
        "ObserveReplacement"
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> crate::error::AgentResult<()> {
        assert_eq!(
            state
                .messages()
                .iter()
                .map(BaseMessage::content)
                .collect::<Vec<_>>(),
            vec!["updated", "added"]
        );
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

async fn assert_before_agent_reconciles_replacement(fail: bool) {
    let mut ctx = make_context();
    let (excluded_id, original_id, original_flags) = {
        let mut transcript = ctx.session.transcript.write();
        let excluded = transcript.append(BaseMessage::human(MessageContent::text("excluded")));
        transcript.set_excluded(excluded, true);
        let original = transcript.append(BaseMessage::human(MessageContent::text("original")));
        transcript.set_truncated(original, true);
        (excluded, original, transcript.flags(original))
    };
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(ReplaceAppendRecall { fail }));
    chain.add(Box::new(ObserveReplacement(Arc::clone(&observed))));
    ctx.runtime.middleware_chain = Arc::new(chain);

    let result = run_before_agent(&ctx).await;
    assert_eq!(result.is_err(), fail);
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), !fail);
    assert_eq!(*ctx.recall_buffer.read(), vec!["replacement recall"]);
    let transcript = ctx.session.transcript.read();
    assert_eq!(
        transcript.len(),
        3,
        "replacement must not append or rebuild entries"
    );
    assert_eq!(transcript.entries()[0].message().id(), excluded_id);
    assert_eq!(transcript.entries()[1].message().id(), original_id);
    assert_eq!(
        transcript.get(original_id).unwrap().message().content(),
        "updated"
    );
    assert_eq!(transcript.flags(original_id), original_flags);
    assert_eq!(
        transcript
            .visible_messages()
            .iter()
            .map(|message| message.content())
            .collect::<Vec<_>>(),
        vec!["updated", "added"]
    );
    assert_eq!(
        transcript.get(excluded_id).unwrap().message().content(),
        "excluded"
    );
}

#[tokio::test]
async fn before_agent_reconciles_stable_id_replacement() {
    assert_before_agent_reconciles_replacement(false).await;
}

#[tokio::test]
async fn before_agent_reconciles_stable_id_replacement_after_error() {
    assert_before_agent_reconciles_replacement(true).await;
}
