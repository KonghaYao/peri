use super::*;
use crate::session::FrozenContext;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, OnBgCompleteFn};

#[tokio::test]
async fn parent_without_custom_callback_still_receives_terminal_and_wake() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let delivery = completion_delivery(Some(&parent), None).unwrap();
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
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .unwrap();
    assert_eq!(parent.queue().len(), 1);
    assert!(parent.queue().drain_all()[0].delivery_id.is_some());
}

#[test]
fn configured_delivery_is_preserved_and_parentless_can_be_execution_only() {
    let configured: OnBgCompleteFn = Arc::new(|_, _| Ok(()));
    assert!(Arc::ptr_eq(
        &configured,
        &completion_delivery(None, Some(Arc::clone(&configured))).unwrap(),
    ));
    assert!(completion_delivery(None, None).is_none());
}
