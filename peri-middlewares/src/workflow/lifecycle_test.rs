use super::*;
use peri_acp_types::tasks::TaskShutdownReport;
use peri_acp_types::tools::ToolContext;
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SCRIPT: &str =
    "export const meta = { name: 'scope', description: 'lifecycle' }; return await agent('wait');";

struct ExecutionDrop {
    dropping: Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Receiver<()>,
    stopped: Arc<AtomicBool>,
}
impl Drop for ExecutionDrop {
    fn drop(&mut self) {
        self.dropping.notify_one();
        self.release
            .recv_timeout(Duration::from_secs(3))
            .expect("test releases agent cleanup");
        self.stopped.store(true, Ordering::Release);
    }
}
struct GatedExecutor {
    entered: Arc<tokio::sync::Notify>,
    cleanup: parking_lot::Mutex<Option<ExecutionDrop>>,
}
#[async_trait]
impl AgentExecutor for GatedExecutor {
    async fn execute(&self, _: AgentRunParams) -> AgentRunResult {
        let _cleanup = self.cleanup.lock().take().unwrap();
        self.entered.notify_one();
        std::future::pending().await
    }
}

fn isolated(test: &str) -> bool {
    if std::env::var("PERI_WORKFLOW_SCOPE_TEST").as_deref() == Ok(test) {
        return false;
    }
    let temp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("PERI_WORKFLOW_SCOPE_TEST", test)
        .env("HOME", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    true
}

fn historical_run(mw: &WorkflowMiddleware) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    mw.journal_store.init_run(&id, SCRIPT).unwrap();
    let state = serde_json::from_value(serde_json::json!({
        "run_id":id,"workflow_name":"scope","status":"killed",
        "script":SCRIPT,"started_at":"2026-09-12T00:00:00Z"
    }))
    .unwrap();
    mw.journal_store.write_state(&id, &state).unwrap();
    id
}

async fn cancel_and_close(resume: bool) {
    let dir = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropping = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let (release, wait_release) = std::sync::mpsc::channel();
    let executor = Arc::new(GatedExecutor {
        entered: entered.clone(),
        cleanup: parking_lot::Mutex::new(Some(ExecutionDrop {
            dropping: dropping.clone(),
            release: wait_release,
            stopped: stopped.clone(),
        })),
    });
    let (tx, _) = tokio::sync::broadcast::channel(8);
    let mw = Arc::new(WorkflowMiddleware::new(
        executor,
        dir.path().to_str().unwrap(),
        tx,
        None,
    ));
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    mw.set_bg_registry(manager.clone());
    let old = historical_run(&mw);
    let mut notifications = mw.subscribe_notifications();
    let launch = mw.clone();
    let invoke = tokio::spawn(async move {
        if resume {
            launch.resume_workflow(&old).await
        } else {
            launch
                .create_tool()
                .invoke(
                    serde_json::json!({"script":SCRIPT}),
                    ToolContext::new(&[], launch.runner.cwd()),
                )
                .await
                .map_err(|error| error.to_string())
        }
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(manager.active_count(), 1);
    let owner = manager.clone();
    let shutdown = tokio::spawn(async move { owner.shutdown().await });
    tokio::time::timeout(Duration::from_secs(3), dropping.notified())
        .await
        .unwrap();
    assert_eq!(
        manager.active_count(),
        0,
        "cancel removes the visible task immediately"
    );
    assert!(!manager.is_execution_idle());
    assert!(
        !shutdown.is_finished(),
        "close must still own the actual agent future"
    );
    assert!(
        notifications.try_recv().is_err(),
        "completion cannot precede agent drain"
    );
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .unwrap()
            .unwrap(),
        TaskShutdownReport::Complete
    );
    assert!(stopped.load(Ordering::Acquire));
    assert!(manager.is_execution_idle());
    let result = tokio::time::timeout(Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.status,
        peri_resources::workflow::registry::WorkflowRunStatus::Killed
    );
    let _ = invoke.await.unwrap();
    assert!(mw
        .create_tool()
        .invoke(
            serde_json::json!({"script":SCRIPT}),
            ToolContext::new(&[], mw.runner.cwd())
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("closing"));
    let prior = historical_run(&mw);
    assert!(mw
        .resume_workflow(&prior)
        .await
        .unwrap_err()
        .contains("closing"));
    assert_eq!(mw.registry.active_count(), 0);
}

#[test]
fn tool_cancel_keeps_session_owned_until_agent_and_node_stop() {
    if isolated(
        "workflow::lifecycle_tests::tool_cancel_keeps_session_owned_until_agent_and_node_stop",
    ) {
        return;
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(cancel_and_close(false));
}

#[test]
fn resume_cancel_keeps_session_owned_until_agent_and_node_stop() {
    if isolated(
        "workflow::lifecycle_tests::resume_cancel_keeps_session_owned_until_agent_and_node_stop",
    ) {
        return;
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(cancel_and_close(true));
}

#[test]
fn execution_manager_binding_cannot_be_replaced() {
    let mw = super::tests::make_middleware();
    let first: Arc<dyn TaskManager> = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let other: Arc<dyn TaskManager> = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    mw.set_bg_registry(first.clone());
    mw.set_bg_registry(first.clone());
    mw.set_bg_registry(other);
    assert!(Arc::ptr_eq(mw.bg_registry.read().as_ref().unwrap(), &first));
}
