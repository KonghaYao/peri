use super::*;
use langfuse_client::{types::TraceBody, IngestionEvent, LangfuseError};
use peri_controller::langfuse::{LangfuseConfig, LangfuseSession, LangfuseSessionLike};
use std::future::Future;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

fn event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

async fn owned_config(
    tmp: &tempfile::TempDir,
    url: &str,
) -> (AcpServerConfig, Arc<LangfuseSession>) {
    let (session, owner) = LangfuseSession::new_owned(
        LangfuseConfig {
            public_key: Some("test-public".into()),
            secret_key: Some("test-secret".into()),
            host: url.into(),
            batch_max_events: 50,
            batch_flush_interval_secs: 60,
            ..Default::default()
        },
        "deployment".into(),
    )
    .await
    .unwrap();
    let mut cfg = test_config(tmp);
    cfg.langfuse_session = Some(Arc::clone(&session));
    cfg.langfuse_shutdown_owner = Some(owner);
    (cfg, session)
}

#[tokio::test]
async fn test_owned_host_shutdown_includes_final_producer_event_and_joins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut http = mockito::Server::new_async().await;
    let request = http
        .mock("POST", "/api/public/otel/v1/traces")
        .match_body(mockito::Matcher::Regex("hosttail".into()))
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (cfg, session) = owned_config(&tmp, &http.url()).await;
    let cancellation = cfg.host_task_spawner.shutdown_token();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let tail = Arc::clone(&session);
    cfg.host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::LegacyCancelHook,
            async move {
                started_tx.send(()).unwrap();
                cancellation.cancelled().await;
                release_rx.await.unwrap();
                tail.try_add(event("hosttail")).unwrap();
            },
        )
        .unwrap();
    let (transport, input, _output) = duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    started_rx.await.unwrap();
    drop(input);
    // 生产者仍有尾部事件；取消 join 等待不应提前关闭 telemetry。
    let mut first = Box::pin(host.shutdown());
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(first);
    session.try_add(event("beforetail")).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), host.shutdown())
            .await
            .unwrap(),
        crate::host::AcpHostShutdownReport::Complete
    );
    assert!(matches!(
        session.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    request.assert_async().await;
}

struct RecoverablePool {
    complete: AtomicBool,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for RecoverablePool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn shutdown(&self) -> peri_acp_types::ports::McpPoolShutdownReport {
        if self.complete.load(Ordering::SeqCst) {
            peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 1,
                failed_services: 0,
            }
        } else {
            peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
                settled_services: 0,
                unfinished_services: 1,
                failed_services: 0,
            }
        }
    }
    fn snapshot(&self) -> serde_json::Value {
        json!({})
    }
}

#[tokio::test]
async fn test_owned_host_incomplete_keeps_telemetry_open_and_retries_same_resources() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut http = mockito::Server::new_async().await;
    let request = http
        .mock("POST", "/api/public/otel/v1/traces")
        .match_body(mockito::Matcher::Regex("afterincomplete".into()))
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (mut cfg, session) = owned_config(&tmp, &http.url()).await;
    let pool = Arc::new(RecoverablePool {
        complete: AtomicBool::new(false),
    });
    cfg.mcp_pool = Some(pool.clone());
    let (transport, input, _output) = duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Incomplete
    );
    session.try_add(event("afterincomplete")).unwrap();
    pool.complete.store(true, Ordering::SeqCst);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    assert!(matches!(
        session.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));
    request.assert_async().await;
}

#[tokio::test]
async fn test_borrowed_host_eof_keeps_external_langfuse_session_open() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut http = mockito::Server::new_async().await;
    let request = http
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (mut cfg, session) = owned_config(&tmp, &http.url()).await;
    let process_owner = cfg.langfuse_shutdown_owner.take().unwrap();
    let (transport, input, _output) = duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    session.try_add(event("anotherhost")).unwrap();
    assert_eq!(
        process_owner.shutdown().await,
        peri_controller::langfuse::LangfuseShutdownReport::Complete
    );
    request.assert_async().await;
}

struct RecoverableSessionClose {
    complete: AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::SessionCloseRegistration for RecoverableSessionClose {
    async fn revoke_and_cleanup(&self) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.complete.load(Ordering::SeqCst) {
            peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
        } else {
            peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Incomplete {
                unfinished_instances: 1,
            }
        }
    }
}

#[tokio::test]
async fn test_owned_host_retries_removed_session_close_owner_before_telemetry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut http = mockito::Server::new_async().await;
    let request = http
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (cfg, session) = owned_config(&tmp, &http.url()).await;
    let manager = cfg.session_manager.clone();
    manager
        .new_session_with_id("closing-session", tmp.path().to_str().unwrap())
        .await
        .unwrap();
    let close = Arc::new(RecoverableSessionClose {
        complete: AtomicBool::new(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    manager
        .get_session_mut("closing-session")
        .unwrap()
        .dynamic_mcp_close = Some(close.clone());
    let (transport, input, _output) = duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Incomplete
    );
    assert!(
        manager.get_session("closing-session").is_none(),
        "会话已从 live registry 移交给退出 owner"
    );
    assert_eq!(close.calls.load(Ordering::SeqCst), 1);
    session.try_add(event("duringretry")).unwrap();
    close.complete.store(true, Ordering::SeqCst);
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    assert_eq!(
        close.calls.load(Ordering::SeqCst),
        2,
        "必须再次调用实际 session close registration，不能仅清计数"
    );
    assert!(matches!(
        session.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));
    assert_eq!(
        host.shutdown().await,
        crate::host::AcpHostShutdownReport::Complete
    );
    assert_eq!(close.calls.load(Ordering::SeqCst), 2);
    request.assert_async().await;
}

#[tokio::test]
async fn test_owned_host_http_failure_keeps_joined_terminal_report() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut http = mockito::Server::new_async().await;
    let request = http
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(400)
        .with_body("private-response-marker")
        .expect(1)
        .create_async()
        .await;
    let (cfg, session) = owned_config(&tmp, &http.url()).await;
    session.try_add(event("failed")).unwrap();
    let (transport, input, _output) = duplex_transport();
    let mut host = crate::host::spawn_acp_server(Arc::new(transport), cfg);
    drop(input);
    let report = host.shutdown().await;
    assert_eq!(
        report,
        crate::host::AcpHostShutdownReport::TelemetryFailed(
            peri_controller::langfuse::LangfuseShutdownReport::DeliveryFailed {
                summary: "1 batch submission(s) failed before the flush barrier".into()
            }
        )
    );
    assert_eq!(host.shutdown().await, report);
    assert!(matches!(
        session.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));
    request.assert_async().await;
}
