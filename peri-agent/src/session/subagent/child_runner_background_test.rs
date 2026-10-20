use super::*;

#[tokio::test(start_paused = true)]
async fn background_child_hands_off_reports_failure() {
    let (child, manager, built, _) = child_with_shell_task(false);
    let child_id = child.store().thread_id.clone().unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = parking_lot::Mutex::new(Some(tx));
    let stops = Arc::new(AtomicUsize::new(0));
    let stop_calls = stops.clone();
    crate::session::subagent::background::spawn_background_subagent(
        "parent-task".into(),
        child_id.clone(),
        "fixture".into(),
        "run shell".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager.clone()),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            if let Some(sender) = tx.lock().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        None,
        None,
        None,
        None,
        Some(Arc::new(move |_, _, _, _, is_error| {
            assert!(is_error, "handoff 必须以错误终态发布 Stop");
            stop_calls.fetch_add(1, Ordering::SeqCst);
        })),
        None,
        None,
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(125), rx)
        .await
        .expect("background child must hand off at D")
        .unwrap();
    assert!(!result.success);
    assert!(!result.timed_out);
    assert_eq!(result.child_thread_id.as_deref(), Some(child_id.as_str()));
    assert!(result.output.contains("status: incomplete"));
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 1, "Shell 保持真实 Running");
}

/// 后台在 D 前完成：成功结果与正常 Stop。
#[tokio::test(start_paused = true)]
async fn background_child_settles_before_deadline_reports_success() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let child_id = child.store().thread_id.clone().unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = parking_lot::Mutex::new(Some(tx));
    let stops = Arc::new(AtomicUsize::new(0));
    let stop_calls = stops.clone();
    crate::session::subagent::background::spawn_background_subagent(
        "parent-task".into(),
        child_id,
        "fixture".into(),
        "initial child work".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            if let Some(sender) = tx.lock().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        None,
        None,
        None,
        None,
        Some(Arc::new(move |_, _, _, _, is_error| {
            assert!(!is_error);
            stop_calls.fetch_add(1, Ordering::SeqCst);
        })),
        None,
        None,
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle_nested_task(&child, &manager);
    let result = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .expect("settled background child must report")
        .unwrap();
    assert!(result.success);
    assert!(result.output.contains("processed nested result"));
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
}

/// 后台协作取消：原身份终态一次，失败语义不伪装成功。
#[tokio::test(start_paused = true)]
async fn background_child_cancellation_reports_interrupted_terminal() {
    let (child, manager, built, _) = child_with_shell_task(false);
    let child_id = child.store().thread_id.clone().unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = parking_lot::Mutex::new(Some(tx));
    let stops = Arc::new(AtomicUsize::new(0));
    let stop_calls = stops.clone();
    let started = Arc::new(AtomicUsize::new(0));
    let started_calls = started.clone();
    crate::session::subagent::background::spawn_background_subagent(
        "parent-task".into(),
        child_id.clone(),
        "fixture".into(),
        "run shell".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            if let Some(sender) = tx.lock().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        None,
        None,
        None,
        Some(Arc::new(move |_, _, _| {
            started_calls.fetch_add(1, Ordering::SeqCst);
        })),
        Some(Arc::new(move |_, _, _, _, is_error| {
            assert!(is_error);
            stop_calls.fetch_add(1, Ordering::SeqCst);
        })),
        None,
        None,
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();
    while started.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    child.config().cancel_token.cancel();
    let mut shell_result = nested_result();
    shell_result.task_id = "shell-task".into();
    assert!(manager.complete("shell-task", shell_result));
    let result = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .expect("cancelled background child must report")
        .unwrap();
    assert!(!result.success);
    assert!(!result.timed_out);
    assert_eq!(result.child_thread_id.as_deref(), Some(child_id.as_str()));
    assert!(result.output.contains("interrupted"));
    assert_eq!(stops.load(Ordering::SeqCst), 1);
}

/// 交接 canonical 写入失败：显式 Failed，不返回 HandedOff 或成功。
struct PanicOnStopBridge;

impl crate::agent::LangfuseBridgeLike for PanicOnStopBridge {
    fn process_render_event(&self, _event: &crate::agent::events_v2::RenderEvent) {}
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            panic!("terminal bridge fixture panic");
        }
    }
}

/// 后台排空失败必须进入 owner 准入，阻止 resume，而不是只改结果文本。
#[tokio::test(start_paused = true)]
async fn background_drain_failure_blocks_resume() {
    let (child, _manager, built, _) = child_with_shell_task(true);
    let child_id = child.store().thread_id.clone().unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = parking_lot::Mutex::new(Some(tx));
    crate::session::subagent::background::spawn_background_subagent(
        "parent-task".into(),
        child_id,
        "fixture".into(),
        "run shell".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            if let Some(sender) = tx.lock().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        Some(Arc::new(PanicOnStopBridge)),
        None,
        None,
        None,
        None,
        None,
        Some(peri_acp_types::identity::AgentId::new()),
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .expect("background child must settle")
        .unwrap();
    assert!(
        !result.success,
        "drain failure must fail the parent result; output={:?} failure={:?}",
        result.output, result.subagent_failure
    );
    assert!(result.output.contains("event drain failed") || result.output.contains("forwarding"));
    let owner = child
        .subagent_host()
        .and_then(|host| host.child_owner.clone())
        .expect("owner retained");
    assert!(
        owner.check_resume().is_err(),
        "排空失败后不得开放新 run 准入"
    );
}

#[test]
fn sync_handoff_routes_late_events_to_background_pump() {
    use crate::agent::events::{AgentEventHandler, ExecutorEvent, FnEventHandler};
    use std::sync::atomic::AtomicBool;

    let foreground = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&foreground);
    let handler: Arc<dyn AgentEventHandler> = Arc::new(FnEventHandler(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let handed_off = Arc::new(AtomicBool::new(false));
    let routed = crate::session::subagent::run_sync::handoff_event_handler(
        Some(handler),
        Some(sender),
        Arc::clone(&handed_off),
    )
    .unwrap();
    let event = || ExecutorEvent::SubagentStarted {
        agent_name: "fixture".into(),
        instance_id: "child".into(),
        parent_tool_call_id: None,
        is_background: false,
    };
    routed.on_event(event());
    assert_eq!(foreground.load(Ordering::SeqCst), 1);
    assert!(receiver.try_recv().is_err());
    handed_off.store(true, Ordering::Release);
    routed.on_event(event());
    assert!(receiver.try_recv().is_ok());
    assert_eq!(foreground.load(Ordering::SeqCst), 1);
}
