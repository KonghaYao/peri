//! 部署关闭权的消费路径：宿主在**任务排空之后**关闭会话存储。
//!
//! 关闭权（`SessionStoreShutdownPort`）不由业务持有，只有部署装配注入宿主配置。这里断言
//! 三件可观察事实：
//!
//! 1. 还有未结束的宿主任务时**不会**动用关闭权；任务排空之后才恰好调用一次；
//! 2. 未确认的关闭不是成功：第一次关闭报 `Incomplete` 并保留上下文，重复关闭重新做
//!    真实检查（端口被再次调用）之后才可能成立；
//! 3. 资源排空超出墙钟预算时立即报 `Incomplete`，但**不跳过**会话存储关闭——退出
//!    已决定，存储必须走自己的有界结清路径，且超时不得伪装成 `Complete`。
//!
//! 端口替身只用于观察「宿主何时调用」；关闭本身的真实语义（结清判定、重复检查、只有
//! 确认关闭才幂等成功）由 peri-resources 的门面测试覆盖。

use crate::host::AcpServerConfig;
use peri_acp_types::ports::{McpTaskOwnerPort, McpTaskShutdownReport};
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceResult, SessionStoreShutdownPort,
};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 关闭权替身：记录调用次数，可按首次必失败注入「未确认的关闭」。
struct RecordingShutdown {
    calls: AtomicUsize,
    fail_first: bool,
}

impl RecordingShutdown {
    fn new(fail_first: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail_first,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// 注入宿主配置的那一份关闭权（`Box<dyn …>` 需要具体类型，观察点仍是同一个计数器）。
struct RecordingShutdownPort(Arc<RecordingShutdown>);

#[async_trait::async_trait]
impl SessionStoreShutdownPort for RecordingShutdownPort {
    async fn shutdown(&self) -> SessionResourceResult<()> {
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_first && call == 0 {
            return Err(SessionResourceError::persistence_uncertain(None));
        }
        Ok(())
    }
}

/// 装配一个带关闭权的宿主配置，并起一条「取消后仍需显式放行」的宿主任务。
///
/// 该任务让「排空尚未完成」成为**可观察的中间态**：宿主已开始关闭，但任务还没结束。
async fn config_with_gated_task(
    tmp: &tempfile::TempDir,
    shutdown: Arc<RecordingShutdown>,
) -> (
    AcpServerConfig,
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let mut cfg = crate::host::stdio::run_server_integration_tests::test_config(tmp).await;
    cfg.session_store_shutdown = Some(Box::new(RecordingShutdownPort(Arc::clone(&shutdown))));
    let cancellation = cfg.host_task_spawner.shutdown_token();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    cfg.host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::UserInputEvents,
            async move {
                started_tx.send(()).unwrap();
                cancellation.cancelled().await;
                release_rx.await.unwrap();
            },
        )
        .unwrap();
    (cfg, release_tx, started_rx)
}

#[tokio::test]
async fn test_store_shutdown_runs_only_after_host_tasks_drain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shutdown = RecordingShutdown::new(false);
    let (cfg, release_tx, started_rx) = config_with_gated_task(&tmp, Arc::clone(&shutdown)).await;
    let (transport, input, _output) =
        crate::host::stdio::run_server_integration_tests::duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    started_rx.await.unwrap();
    // 传输 EOF：宿主开始关闭，但被门控的宿主任务还没结束。
    drop(input);
    let mut first = Box::pin(host.shutdown());
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(shutdown.calls(), 0, "任务尚未排空时不得动用部署关闭权");
    drop(first);

    release_tx.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), host.shutdown())
            .await
            .unwrap(),
        crate::host::AcpHostShutdownReport::Complete
    );
    // 排空之后恰好一次；重复观察终态不再重复关闭。
    assert_eq!(shutdown.calls(), 1);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    assert_eq!(shutdown.calls(), 1);
}

#[tokio::test]
async fn test_unconfirmed_store_shutdown_is_incomplete_and_retry_rechecks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shutdown = RecordingShutdown::new(true);
    let cfg = {
        let mut cfg = crate::host::stdio::run_server_integration_tests::test_config(&tmp).await;
        cfg.session_store_shutdown = Some(Box::new(RecordingShutdownPort(Arc::clone(&shutdown))));
        cfg
    };
    let (transport, input, _output) =
        crate::host::stdio::run_server_integration_tests::duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);

    // 未确认的关闭不是完成：报告未完成，部署保留上下文。
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), host.shutdown())
            .await
            .unwrap(),
        crate::host::AcpHostShutdownReport::Incomplete
    );
    assert_eq!(shutdown.calls(), 1);
    // 重复关闭重新做真实检查（端口被再次调用），这次才成立。
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), host.shutdown())
            .await
            .unwrap(),
        crate::host::AcpHostShutdownReport::Complete
    );
    assert_eq!(shutdown.calls(), 2);
}

/// 慢 MCP task owner：把关闭卡在整个预算之外，证明预算到期会返回而不是挂起。
struct SlowMcpTaskOwner;

#[async_trait::async_trait]
impl McpTaskOwnerPort for SlowMcpTaskOwner {
    fn begin_shutdown(&self) {}

    async fn shutdown(&mut self) -> McpTaskShutdownReport {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        McpTaskShutdownReport::Complete
    }
}

#[tokio::test]
async fn test_budget_expiry_reports_incomplete_and_still_closes_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shutdown = RecordingShutdown::new(false);
    let cfg = {
        let mut cfg = crate::host::stdio::run_server_integration_tests::test_config(&tmp).await;
        cfg.mcp_task_owner = Some(Box::new(SlowMcpTaskOwner));
        cfg.session_store_shutdown = Some(Box::new(RecordingShutdownPort(Arc::clone(&shutdown))));
        cfg
    };
    let (transport, input, _output) =
        crate::host::stdio::run_server_integration_tests::duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);

    let started = std::time::Instant::now();
    let report = tokio::time::timeout(std::time::Duration::from_secs(15), host.shutdown())
        .await
        .expect("shutdown must be bounded by the wall-clock budget");
    assert_eq!(report, crate::host::AcpHostShutdownReport::Incomplete);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "budget must bound the drain: {:?}",
        started.elapsed()
    );
    assert_eq!(
        shutdown.calls(),
        1,
        "budget expiry must not skip the session store close"
    );
}
