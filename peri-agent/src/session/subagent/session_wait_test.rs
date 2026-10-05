use super::*;
use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{Reasoning, StreamingContext};
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::session::subagent::SubagentHost;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct ResultLlm(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for ResultLlm {
    async fn generate_reasoning(
        &self,
        messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        assert!(messages
            .iter()
            .any(|message| message.content().contains("child-result")));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Reasoning::with_answer("", "processed"))
    }
}

#[tokio::test]
async fn child_idle_loop_processes_its_terminal_result_without_parent_loop() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let manager = Arc::new(TaskManager::new());
    manager
        .register_with_kind(BackgroundTask {
            id: "child-task".into(),
            agent_name: "fixture".into(),
            prompt_summary: "child work".into(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: peri_time::now_wall().into(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Abort(tokio::spawn(async {})),
            cancel_token: None,
            pid: None,
            output_preview: None,
            agent_inbox: None,
            initiator_session_id: Some(child_id.clone()),
            owner_session_id: Some(child_id.clone()),
            owner_identity: None,
        })
        .unwrap();
    let router = crate::session::async_router::AsyncRouter::for_queue(child.queue());
    let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, kind| {
        router.route_bg_result(result, kind);
        Ok(())
    });
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        on_bg_complete: Some(callback.clone()),
        ..Default::default()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ResultLlm(calls.clone())),
        MiddlewareChain::new(),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    let suspended = Arc::new(AtomicBool::new(false));
    built.context.async_ctx.idle_suspended_flag = Some(suspended.clone());
    let running = tokio::spawn(run_react_loop(built.context, 20));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !suspended.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!running.is_finished());
    manager
        .settle_completed(
            "child-task",
            BackgroundTaskResult {
                task_id: "child-task".into(),
                agent_name: "fixture".into(),
                prompt_summary: "child work".into(),
                success: true,
                output: "child-result".into(),
                tool_calls_count: 0,
                duration_ms: 1,
                child_thread_id: None,
                timed_out: false,
                subagent_failure: None,
                shell_output: None,
            },
            callback,
        )
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(parent.queue().is_empty());
    assert!(parent.transcript().read().visible_messages().is_empty());
    assert!(child
        .transcript()
        .read()
        .visible_messages()
        .iter()
        .any(|message| message.content().contains("processed")));
}
