use crate::session::FrozenContext;
use crate::session::Session;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, OnBgCompleteFn};
use std::sync::Arc;

#[tokio::test]
async fn parent_without_custom_callback_still_receives_terminal_and_wake() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let delivery = crate::session::subagent::factory::completion_delivery(Some(&parent), None)
        .expect("the completion delivery fixture result is required for this assertion");
    let waiting = parent.queue().await_wake();
    tokio::pin!(waiting);
    futures::future::poll_fn(|context| {
        use std::future::Future;
        assert!(waiting.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    delivery(
        &BackgroundTaskResult {
            task_id: "task".into(),
            agent_name: "fixture".into(),
            prompt_summary: "task".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 0,
            child_thread_id: Some("child".into()),
            timed_out: false,
            subagent_failure: None,
            shell_output: None,
        },
        BgTaskKind::Agent,
    )
    .expect("the delivery fixture result is required for this assertion");
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("the timeout fixture result is required for this assertion");
    assert_eq!(parent.queue().len_sync(), 1);
    assert!(parent.queue().drain_all_sync()[0].delivery_id.is_some());
}

#[test]
fn configured_delivery_is_preserved_and_parentless_can_be_execution_only() {
    let configured: OnBgCompleteFn = Arc::new(|_, _| Ok(()));
    assert!(Arc::ptr_eq(
        &configured,
        &crate::session::subagent::factory::completion_delivery(
            None,
            Some(Arc::clone(&configured))
        )
        .expect("the clone fixture result is required for this assertion"),
    ));
    assert!(crate::session::subagent::factory::completion_delivery(None, None).is_none());
}

#[tokio::test]
async fn completion_target_follows_known_callback_and_preserves_original_parent() {
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("b".into()),
    );
    let callback: OnBgCompleteFn = Arc::new(|_, _| Ok(()));
    parent.set_subagent_host(crate::session::subagent::SubagentHost {
        on_bg_complete: Some(callback.clone()),
        completion_target: Some("configured-recipient".into()),
        ..Default::default()
    });
    assert_eq!(
        crate::session::subagent::factory::completion_target(
            Some(&parent),
            Some(&callback),
            Some("b".into())
        )
        .as_deref(),
        Some("configured-recipient")
    );
    let opaque: OnBgCompleteFn = Arc::new(|_, _| Ok(()));
    assert_eq!(
        crate::session::subagent::factory::completion_target(
            Some(&parent),
            Some(&opaque),
            Some("b".into())
        ),
        None
    );
    assert_eq!(
        crate::session::subagent::factory::completion_target(Some(&parent), None, Some("b".into()))
            .as_deref(),
        Some("b")
    );

    let test = crate::session::test_resources::TestSession::open().await;
    let store = test.resources();
    let original = test.thread_id();
    let child = test.create_child(&original).await;
    crate::session::subagent::factory::persist_completion_target(store.as_ref(), &child, Some("b"))
        .await
        .expect("the persist completion target fixture result is required for this assertion");
    let meta = store
        .load_session_meta(&child)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.parent_thread_id.as_ref(), Some(&original));
    assert_eq!(
        peri_acp_types::thread::completion_target(meta.config.as_deref())
            .expect("the as deref fixture result is required for this assertion"),
        "b"
    );
}
