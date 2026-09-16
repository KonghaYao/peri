//! Real shell regression tests for the session directory and process-tree owner.
use crate::hooks::{
    dispatcher::{fire_standalone_lifecycle_hooks_owned, HookDispatcher},
    executor::execute_command_hook_owned,
    once_tracker::OnceTracker,
    types::{HookAction, HookEvent, HookInput, RegisteredHook},
};
use peri_acp_types::tasks::{TaskManager, TaskShutdownReport};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn hook(command: &str, event: HookEvent, asynchronous: bool, timeout: u64) -> RegisteredHook {
    RegisteredHook {
        hook: serde_json::from_value(serde_json::json!({
            "type":"command", "command":command, "async":asynchronous, "timeout":timeout,
        }))
        .unwrap(),
        event,
        matcher: None,
        plugin_name: "fixture".into(),
        plugin_id: "fixture".into(),
        plugin_root: PathBuf::new(),
        plugin_data_dir: PathBuf::new(),
        plugin_options: HashMap::new(),
    }
}
fn manager() -> Arc<dyn TaskManager> {
    Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
}
fn input(cwd: &Path) -> HookInput {
    HookInput::session_start(
        "workspace-session",
        "",
        cwd.to_str().unwrap(),
        "startup",
        "test",
    )
}
fn assert_block(action: HookAction, expected: &str) {
    match action {
        HookAction::Block { reason } => assert_eq!(reason, expected),
        other => panic!("expected hook output {expected:?}, got {other:?}"),
    }
}
async fn await_pid(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(path) {
                if let Ok(pid) = value.trim().parse() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hook writes its process identity")
}
async fn assert_group_stopped(pid: u32) {
    let status = peri_agent::agent::async_tasks::shell_command(&format!("kill -0 -- -{pid}"), &[])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(
        !status.success(),
        "close returned while hook process group {pid} was alive"
    );
}
const SLOW_COMMAND: &str =
    "sleep 30 & printf '%s' \"$!\" > child.pid; printf '%s' \"$$\" > leader.pid; wait";

#[tokio::test]
async fn command_executes_relative_paths_in_its_session_directory() {
    let cwd = tempfile::tempdir().unwrap();
    let canonical = cwd.path().canonicalize().unwrap();
    let manager = manager();
    let event_input = input(&canonical);
    for (command, expected) in [
        ("pwd; exit 2".to_string(), canonical.display().to_string()),
        (
            "printf '%s' \"$CLAUDE_PROJECT_DIR\"; exit 2".to_string(),
            canonical.display().to_string(),
        ),
    ] {
        let registered = hook(&command, HookEvent::SessionStart, false, 5);
        let action = execute_command_hook_owned(
            &registered.hook,
            &event_input,
            &registered,
            Some(manager.as_ref()),
        )
        .await;
        assert_block(action, &expected);
    }
    let relative = format!("hook-context-{}", uuid::Uuid::now_v7());
    std::fs::write(canonical.join(&relative), "workspace B").unwrap();
    let registered = hook(
        &format!("cat {relative}; exit 2"),
        HookEvent::SessionStart,
        false,
        5,
    );
    assert_block(
        execute_command_hook_owned(
            &registered.hook,
            &event_input,
            &registered,
            Some(manager.as_ref()),
        )
        .await,
        "workspace B",
    );
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn sync_hook_cancel_reaps_descendants_before_session_close() {
    let cwd = tempfile::tempdir().unwrap();
    let event_input = input(cwd.path());
    let registered = hook(SLOW_COMMAND, HookEvent::SessionStart, false, 60);
    let manager = manager();
    let owner = manager.clone();
    let task = tokio::spawn(async move {
        execute_command_hook_owned(
            &registered.hook,
            &event_input,
            &registered,
            Some(owner.as_ref()),
        )
        .await
    });
    let leader = await_pid(&cwd.path().join("leader.pid")).await;
    let _child = await_pid(&cwd.path().join("child.pid")).await;
    assert!(!manager.is_execution_idle());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert!(matches!(task.await.unwrap(), HookAction::Allow));
    assert_group_stopped(leader).await;
}

#[tokio::test]
async fn dropped_sync_hook_future_retains_process_cleanup_ownership() {
    let cwd = tempfile::tempdir().unwrap();
    let event_input = input(cwd.path());
    let registered = hook(SLOW_COMMAND, HookEvent::SessionStart, false, 60);
    let manager = manager();
    let owner = manager.clone();
    let task = tokio::spawn(async move {
        execute_command_hook_owned(
            &registered.hook,
            &event_input,
            &registered,
            Some(owner.as_ref()),
        )
        .await
    });
    let leader = await_pid(&cwd.path().join("leader.pid")).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert_group_stopped(leader).await;
}

#[tokio::test]
async fn async_dispatcher_hook_is_cancelled_and_drained_by_its_session() {
    let cwd = tempfile::tempdir().unwrap();
    let registered = hook(SLOW_COMMAND, HookEvent::SessionStart, true, 60);
    let mut hooks = HashMap::new();
    hooks.insert(HookEvent::SessionStart, vec![registered]);
    let manager = manager();
    let dispatcher = HookDispatcher::new(
        Arc::new(parking_lot::RwLock::new(hooks)),
        Arc::new(|| panic!("command hook cannot instantiate an LLM")),
        Arc::new(OnceTracker::new()),
        cwd.path().display().to_string(),
    )
    .with_task_manager(manager.clone());
    assert!(matches!(
        dispatcher
            .fire_event(HookEvent::SessionStart, &input(cwd.path()), None, None)
            .await,
        HookAction::Allow
    ));
    let leader = await_pid(&cwd.path().join("leader.pid")).await;
    assert!(!manager.is_execution_idle());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert_group_stopped(leader).await;
    std::fs::remove_file(cwd.path().join("leader.pid")).unwrap();
    dispatcher
        .fire_event(HookEvent::SessionStart, &input(cwd.path()), None, None)
        .await;
    assert!(
        !cwd.path().join("leader.pid").exists(),
        "closing scope rejects new async hooks"
    );
}

#[tokio::test]
async fn standalone_async_compact_hook_uses_the_same_session_owner() {
    let cwd = tempfile::tempdir().unwrap();
    let registered = hook(SLOW_COMMAND, HookEvent::PreCompact, true, 60);
    let manager = manager();
    super::stage_firing::fire_pre_compact(
        &[registered],
        cwd.path().to_str().unwrap(),
        "session",
        "",
        "test",
        0,
        Some(manager.clone()),
    )
    .await;
    let leader = await_pid(&cwd.path().join("leader.pid")).await;
    assert!(!manager.is_execution_idle());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert_group_stopped(leader).await;
}

#[tokio::test]
async fn timeout_keeps_allow_semantics_but_drains_the_process_group() {
    let cwd = tempfile::tempdir().unwrap();
    let registered = hook(SLOW_COMMAND, HookEvent::SessionStart, false, 1);
    let manager = manager();
    let action = execute_command_hook_owned(
        &registered.hook,
        &input(cwd.path()),
        &registered,
        Some(manager.as_ref()),
    )
    .await;
    assert!(matches!(action, HookAction::Allow));
    let leader = await_pid(&cwd.path().join("leader.pid")).await;
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert_group_stopped(leader).await;
}

#[tokio::test]
async fn session_end_waits_for_an_async_hook_before_returning() {
    let cwd = tempfile::tempdir().unwrap();
    let registered = hook(
        "sleep 0.1; printf 'ended' > ended",
        HookEvent::SessionEnd,
        true,
        5,
    );
    let manager = manager();
    fire_standalone_lifecycle_hooks_owned(
        &[registered],
        HookEvent::SessionEnd,
        cwd.path().to_str().unwrap(),
        "session",
        "",
        "test",
        None,
        Some("close"),
        Some(manager.clone()),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("ended")).unwrap(),
        "ended"
    );
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}
