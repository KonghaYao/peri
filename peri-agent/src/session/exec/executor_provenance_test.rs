//! Hidden child history must retain its inherited boundary at the root executor seam.
use super::*;
use crate::agent::stages::StageContext;
use crate::session::exec::stage_builder::V2AgentOutput;
use crate::session::{FrozenContext, Session};
use crate::thread::{SqliteThreadStore, ThreadMeta, ThreadStore};
use peri_acp_types::event_v2::EventBus;
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};

#[tokio::test]
async fn test_hidden_child_executor_restores_ancestor_and_own_flags_separately() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteThreadStore::new(dir.path().join("child.db"))
            .await
            .unwrap(),
    );
    let parent_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let parent = BaseMessage::human("parent snapshot");
    store
        .append_messages(&parent_id, std::slice::from_ref(&parent))
        .await
        .unwrap();
    let parent_flags = MessageFlags {
        excluded: true,
        ..Default::default()
    };
    store
        .update_message_flags(&parent.id(), &parent_flags)
        .await
        .unwrap();
    let mut meta = ThreadMeta::new("/tmp");
    meta.parent_thread_id = Some(parent_id.clone());
    meta.snapshot_at_message_id = Some(parent.id().as_uuid().to_string());
    meta.hidden = true;
    let child_id = store.create_thread(meta).await.unwrap();
    store
        .store_inherited_context(
            &child_id,
            &InheritedContext {
                payloads: vec![PersistedPayload::Message(parent.clone())],
                flags: std::collections::HashMap::from([(parent.id(), parent_flags.clone())]),
            },
        )
        .await
        .unwrap();
    let own = BaseMessage::human("child old history");
    store
        .append_messages(&child_id, std::slice::from_ref(&own))
        .await
        .unwrap();
    let own_flags = MessageFlags {
        truncated: true,
        ..Default::default()
    };
    store
        .update_message_flags(&own.id(), &own_flags)
        .await
        .unwrap();
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let inspected = session.clone();
    let stage_build: StageBuildFn = Arc::new(move |_| {
        let turn = session.start_turn();
        turn.cancel_token.cancel();
        let (bus, handles) = EventBus::new(Default::default());
        let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_event_bus(Arc::new(bus))
            .build();
        let (_, todo_rx) = tokio::sync::mpsc::channel(8);
        let (_, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
        Ok((
            V2AgentOutput {
                context,
                session: session.clone(),
                event_handles: handles,
                todo_rx,
                bg_event_rx,
            },
            None,
        ))
    });
    let mut context = make_session_context("hidden-child-provenance");
    context.thread_id = Some(child_id.clone());
    context.thread_store = Some(store.clone());
    let mut turn = make_turn_input(
        Arc::new(MockEventSink::new()),
        MessageContent::text("continue"),
        false,
        store.load_context(&child_id).await.unwrap(),
    );
    turn.stage_build = stage_build;
    let result = run_session_loop(context, turn).await;
    assert!(!result.ok, "预取消 Stage 应正常以 Interrupted 退出");
    {
        let arc = inspected.transcript();
        let mut transcript = arc.write();
        assert_eq!(transcript.ancestor_len(), 1);
        assert_eq!(transcript.entries()[0].id(), parent.id());
        assert_eq!(transcript.entries()[1].id(), own.id());
        assert_eq!(transcript.flags(parent.id()), parent_flags);
        assert_eq!(transcript.flags(own.id()), own_flags);
        transcript.set_excluded(parent.id(), false);
        assert_eq!(
            transcript.flags(parent.id()),
            parent_flags,
            "hidden child 不得改 parent flags"
        );
    }
    assert_eq!(
        store.load_message_flags(&parent_id).await.unwrap()[&parent.id()],
        parent_flags
    );
    store.close().await;
}
