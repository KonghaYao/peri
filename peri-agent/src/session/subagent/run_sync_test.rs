use super::*;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::middleware::chain::MiddlewareChain;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::subagent::SubagentHost;
use crate::session::FrozenContext;
use crate::tools::BaseTool;
use peri_acp_types::tasks::BgTaskRegistration;
use std::sync::atomic::{AtomicUsize, Ordering};

fn child(manager: Arc<TaskManager>) -> Arc<Session> {
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(uuid::Uuid::now_v7().to_string()),
    );
    session.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        ..Default::default()
    });
    session
}

struct TerminalLlm;

struct NestedParentLlm(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for NestedParentLlm {
    async fn generate_reasoning(
        &self,
        messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        if messages
            .iter()
            .any(|message| message.content().contains("C completed"))
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "B summarized C completed"))
        } else {
            Ok(Reasoning::with_answer("", "B waiting for C"))
        }
    }
}

#[async_trait::async_trait]
impl ReactLLM for TerminalLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        Ok(Reasoning::with_answer("", "C completed"))
    }
}

#[tokio::test(start_paused = true)]
async fn nested_a_b_c_handoff_settles_each_parent_once_and_resume_has_distinct_ids() {
    let a_manager = Arc::new(TaskManager::new());
    let b_manager = Arc::new(TaskManager::new());
    let b = child(b_manager.clone());
    let b_thread_id = b.store().thread_id.clone().unwrap();
    let a_deliveries = Arc::new(AtomicUsize::new(0));
    let a_attempts = Arc::new(AtomicUsize::new(0));
    let b_deliveries = Arc::new(AtomicUsize::new(0));
    let mut previous = None;

    for _ in 0..2 {
        let c_manager = Arc::new(TaskManager::new());
        let c = child(c_manager.clone());
        let c_thread_id = c.store().thread_id.clone().unwrap();
        c.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            crate::messages::BaseMessage::human("finish C after the shell settles"),
        ));
        let c_context = super::super::v2_bridge::build_v2_subagent_context(
            Some(c.clone()),
            Box::new(TerminalLlm),
            Arc::new(MiddlewareChain::new()),
            Vec::new(),
            Arc::new(|_| true),
            None,
            "/tmp",
            c.config().cancel_token.as_ref().clone(),
            None,
            None,
            None,
            None,
            Some(super::super::v2_bridge::agent_id_from_child_thread(
                &c_thread_id,
            )),
        );
        let shell_task_id = uuid::Uuid::now_v7().to_string();
        c_manager
            .register(BgTaskRegistration {
                task_id: shell_task_id.clone(),
                kind: BgTaskKind::Shell,
                summary: "C's unresolved shell".into(),
                pid: None,
                kill: Some(Box::new(|| {})),
            })
            .unwrap();
        let b_task_id = new_sync_task_id();
        let a_task_id = new_sync_task_id();
        assert_ne!(previous.as_ref(), Some(&a_task_id));
        previous = Some(a_task_id.clone());
        let a_calls = a_deliveries.clone();
        let attempts = a_attempts.clone();
        let delivered_a_task_id = a_task_id.clone();
        let b_calls = b_deliveries.clone();
        let delivered_b_task_id = b_task_id.clone();
        let running_b_manager = b_manager.clone();
        let running_c = c.clone();
        let running_a_manager = a_manager.clone();
        let mut a_call = tokio::spawn(run_sync_with_registered_handoff(
            b.clone(),
            b_thread_id.clone(),
            "B".into(),
            Some(running_a_manager),
            Some(a_task_id.clone()),
            Some(Arc::new(move |result, _| {
                assert!(result.success);
                assert_eq!(result.task_id, delivered_a_task_id);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err("temporary rejection".into());
                }
                a_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            move |handoff_b, _| async move {
                let c_id = c_thread_id.clone();
                let c_result = run_sync_with_registered_handoff(
                    running_c,
                    c_id,
                    "C".into(),
                    Some(running_b_manager.clone()),
                    Some(b_task_id),
                    Some(Arc::new(move |result, _| {
                        assert!(result.success);
                        assert_eq!(result.task_id, delivered_b_task_id);
                        b_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })),
                    move |handoff_c, _| async move {
                        run_sync_subagent(
                            &c_thread_id,
                            "C",
                            "/tmp",
                            10,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            c_context,
                            c,
                            None,
                            Some(handoff_c),
                        )
                        .await
                    },
                )
                .await?;
                assert!(matches!(c_result, SyncCompletion::Pending(_)));
                let _ = handoff_b.send(());
                while running_b_manager.active_count() != 0 {
                    peri_time::sleep(Duration::from_secs(1)).await;
                }
                Ok(false)
            },
        ));
        let outcome = tokio::time::timeout(Duration::from_secs(250), &mut a_call)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let SyncCompletion::Pending(pending) = outcome else {
            panic!("A must retain B's pending execution");
        };
        assert!(!pending.is_empty());
        assert_eq!(a_manager.active_count(), 1);
        assert_eq!(b_manager.active_count(), 1);
        assert!(a_manager
            .snapshot()
            .tasks
            .iter()
            .any(|task| task.task_id == a_task_id));
        let pending_result = super::super::types::SubagentSpawned {
            child_thread_id: b_thread_id.clone(),
            task_id: Some(a_task_id.clone()),
            session: b.clone(),
            cancel_token: b.config().cancel_token.as_ref().clone(),
            interrupted: false,
            pending_tasks: pending,
        };
        assert!(pending_result
            .pending_message()
            .unwrap()
            .contains(&format!("task_id: {a_task_id}")));
        assert_eq!(
            uuid::Uuid::parse_str(a_task_id.strip_prefix("sync-").unwrap())
                .unwrap()
                .get_version_num(),
            7
        );
        let result = peri_acp_types::event::BackgroundTaskResult {
            task_id: shell_task_id.clone(),
            agent_name: "shell".into(),
            prompt_summary: "shell".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: false,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        };
        assert!(c_manager.complete(&shell_task_id, result));
        tokio::time::timeout(Duration::from_secs(10), async {
            while a_manager.active_count() != 0 {
                peri_time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            a_deliveries.load(Ordering::SeqCst),
            b_deliveries.load(Ordering::SeqCst)
        );
    }
    assert_eq!(a_deliveries.load(Ordering::SeqCst), 2);
    assert_eq!(a_attempts.load(Ordering::SeqCst), 3);
    assert_eq!(b_deliveries.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn missing_parent_task_directory_cannot_return_pending() {
    let child_manager = Arc::new(TaskManager::new());
    let session = child(child_manager.clone());
    child_manager
        .register(BgTaskRegistration {
            task_id: "unsettled".into(),
            kind: BgTaskKind::Shell,
            summary: "unsettled".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let closed_manager = Arc::new(TaskManager::new());
    closed_manager.begin_session_close();
    for parent_manager in [None, Some(closed_manager)] {
        let (release, wait) = oneshot::channel::<()>();
        let mut call = tokio::spawn(run_sync_with_registered_handoff(
            session.clone(),
            session.store().thread_id.clone().unwrap(),
            "child".into(),
            parent_manager,
            Some(new_sync_task_id()),
            Some(Arc::new(|_, _| Ok(()))),
            move |handoff, _| async move {
                let _ = handoff.send(());
                let _ = wait.await;
                Ok(false)
            },
        ));
        assert!(tokio::time::timeout(Duration::from_secs(121), &mut call)
            .await
            .is_err());
        release.send(()).unwrap();
        assert!(matches!(
            call.await.unwrap().unwrap(),
            SyncCompletion::Finished(false)
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn parent_scope_close_does_not_discard_rejected_delivery() {
    let parent_manager = Arc::new(TaskManager::new());
    let child_manager = Arc::new(TaskManager::new());
    let session = child(child_manager.clone());
    child_manager
        .register(BgTaskRegistration {
            task_id: "unsettled".into(),
            kind: BgTaskKind::Shell,
            summary: "unsettled".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let (release, wait) = oneshot::channel::<()>();
    let attempts = Arc::new(AtomicUsize::new(0));
    let callback_attempts = attempts.clone();
    let task_id = new_sync_task_id();
    let mut call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        session.store().thread_id.clone().unwrap(),
        "child".into(),
        Some(parent_manager.clone()),
        Some(task_id.clone()),
        Some(Arc::new(move |result, _| {
            assert_eq!(result.task_id, task_id);
            if callback_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("retry after scope close".into())
            } else {
                Ok(())
            }
        })),
        move |handoff, _| async move {
            let _ = handoff.send(());
            let _ = wait.await;
            Ok(false)
        },
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(121), &mut call)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        SyncCompletion::Pending(_)
    ));
    parent_manager.begin_session_close();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        while parent_manager.active_count() != 0 {
            peri_time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn nested_child_result_is_consumed_by_parent_runner_before_root_completion() {
    let root_manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let nested_manager = Arc::new(TaskManager::new());
    let parent = child(parent_manager.clone());
    let nested = child(nested_manager.clone());
    let parent_id = parent.store().thread_id.clone().unwrap();
    let nested_id = nested.store().thread_id.clone().unwrap();
    let nested_task_id = new_sync_task_id();
    let root_task_id = new_sync_task_id();
    let nested_shell = "nested-shell";
    nested_manager
        .register(BgTaskRegistration {
            task_id: nested_shell.into(),
            kind: BgTaskKind::Shell,
            summary: "late shell".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    nested.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("C initial task"),
    ));
    let nested_context = super::super::v2_bridge::build_v2_subagent_context(
        Some(nested.clone()),
        Box::new(TerminalLlm),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        nested.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(super::super::v2_bridge::agent_id_from_child_thread(
            &nested_id,
        )),
    );
    let nested_run_id = nested_id.clone();
    let inbox = crate::session::async_router::AsyncRouter::for_queue(parent.queue());
    let pending_nested = run_sync_with_registered_handoff(
        nested.clone(),
        nested_id,
        "C".into(),
        Some(parent_manager.clone()),
        Some(nested_task_id),
        Some(Arc::new(move |result, kind| {
            inbox.route_bg_result(result, kind);
            Ok(())
        })),
        move |handoff, _| async move {
            run_sync_subagent(
                &nested_run_id,
                "C",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                nested_context,
                nested,
                None,
                Some(handoff),
            )
            .await
        },
    )
    .await
    .unwrap();
    assert!(matches!(pending_nested, SyncCompletion::Pending(_)));
    assert_eq!(parent_manager.active_count(), 1);

    parent.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("B should wait for C"),
    ));
    let consumed = Arc::new(AtomicUsize::new(0));
    let parent_context = super::super::v2_bridge::build_v2_subagent_context(
        Some(parent.clone()),
        Box::new(NestedParentLlm(consumed.clone())),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        parent.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(super::super::v2_bridge::agent_id_from_child_thread(
            &parent_id,
        )),
    );
    let (root_sender, root_receiver) = oneshot::channel();
    let root_sender = parking_lot::Mutex::new(Some(root_sender));
    let run_parent_id = parent_id.clone();
    let parent_session = parent.clone();
    let mut parent_call = tokio::spawn(run_sync_with_registered_handoff(
        parent,
        parent_id,
        "B".into(),
        Some(root_manager.clone()),
        Some(root_task_id),
        Some(Arc::new(move |result, _| {
            if let Some(sender) = root_sender.lock().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_parent_id,
                "B",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                parent_context,
                parent_session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(121), &mut parent_call)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        SyncCompletion::Pending(_)
    ));
    assert_eq!(consumed.load(Ordering::SeqCst), 0);
    assert_eq!(root_manager.active_count(), 1);
    assert!(nested_manager.complete(
        nested_shell,
        peri_acp_types::event::BackgroundTaskResult {
            task_id: nested_shell.into(),
            agent_name: "shell".into(),
            prompt_summary: "late shell".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: false,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        }
    ));
    let completed = tokio::time::timeout(Duration::from_secs(10), root_receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(completed.success);
    assert!(completed.output.contains("B summarized C completed"));
    assert!(consumed.load(Ordering::SeqCst) > 0);
    assert_eq!(parent_manager.active_count(), 0);
    assert_eq!(root_manager.active_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn explicit_close_stops_failed_delivery_retry_without_reporting_success() {
    let manager = Arc::new(TaskManager::new());
    let session = child(manager.clone());
    manager
        .register(BgTaskRegistration {
            task_id: "shell".into(),
            kind: BgTaskKind::Shell,
            summary: "pending".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let (release, waiting) = oneshot::channel::<()>();
    let mut call = tokio::spawn(run_sync_with_handoff(
        session.clone(),
        session.store().thread_id.clone().unwrap(),
        "child".into(),
        Some(Arc::new(move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Err("delivery unavailable".into())
        })),
        move |handoff, _| async move {
            handoff.send(()).unwrap();
            waiting.await.unwrap();
            Ok(false)
        },
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(121), &mut call)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        SyncCompletion::Pending(_)
    ));
    assert!(manager.complete(
        "shell",
        peri_acp_types::event::BackgroundTaskResult {
            task_id: "shell".into(),
            agent_name: "shell".into(),
            prompt_summary: "pending".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: false,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        }
    ));
    release.send(()).unwrap();
    while attempts.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    let error = super::super::close::close_subagent_session_scope(session.clone())
        .await
        .unwrap_err();
    assert!(error.contains("delivery unavailable"), "{error}");
    tokio::time::timeout(
        Duration::from_secs(1),
        session
            .subagent_host()
            .unwrap()
            .close_state
            .sync_workers
            .wait(),
    )
    .await
    .expect("close must stop the retry worker");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(super::super::close::close_subagent_session_scope(session)
        .await
        .is_err());
}

#[test]
fn resumed_child_handoffs_have_distinct_canonical_delivery_identities() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::session::bg_complete::queue_terminal_delivery(parent.queue().clone()),
    );
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut identities = Vec::new();
    for output in ["first result", "second result", "second result"] {
        let task_id = new_sync_task_id();
        let result = peri_acp_types::event::BackgroundTaskResult {
            task_id: task_id.clone(),
            agent_name: "fixture".into(),
            prompt_summary: "resume".into(),
            success: true,
            output: output.into(),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: false,
            child_thread_id: Some(child_id.clone()),
            subagent_failure: None,
            shell_output: None,
        };
        delivery(&result, BgTaskKind::Agent).unwrap();
        delivery(&result, BgTaskKind::Agent).unwrap();
        identities.push(task_id);
    }
    assert_eq!(identities.len(), 3);
    assert_ne!(identities[0], identities[1]);
    assert_ne!(identities[1], identities[2]);
    let messages = parent.queue().drain_all();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages
            .iter()
            .filter_map(|message| message.delivery_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
}
