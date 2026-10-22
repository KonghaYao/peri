//! 跨 server 控制与连接提交的锁顺序回归；同步闩锁在失败断言前释放。
use super::*;
use crate::mcp::client::McpClientHandle;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const LOCK_WAIT: Duration = Duration::from_secs(5);

fn connected_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: name.into(),
        version: None,
        cache_version: None,
        connected_at: None,
        protocol_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

fn control_and_commit_at_service_handoff(action: &str) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (mut owner, pool, _cwd) = pool_fixture();
    let McpControlRequest::Register { config, .. } = wire_registration("controlled") else {
        unreachable!()
    };
    pool.configs
        .write()
        .insert("controlled".into(), config.into_server_config());
    let old_handle = connected_handle("controlled");
    assert!(pool
        .try_commit_connection(
            "controlled".into(),
            old_handle.clone(),
            McpServiceWrapper::Closed,
        )
        .is_ok());
    let slot = pool.server_slot("controlled");
    // owner 登记后、slot 保留前暂停：旧 if-let 实现会跨此停点持 services 锁。
    let retention_latch = slot.services.lock();
    let control_pool = pool.clone();
    let control_request = request(action, "controlled");
    let (control_tx, control_rx) = mpsc::channel();
    let control_task = runtime.spawn(async move {
        let result = control_pool.control_mcp(control_request).await;
        let _ = control_tx.send(result);
    });
    // 观察真实 owner 发布，不能用 sleep 猜测控制任务已运行到交接边界。
    let deadline = Instant::now() + LOCK_WAIT;
    let handoff_reached = loop {
        if pool.retained_service_count() == 1 {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };
    let other_handle = connected_handle("other");
    let commit_handle = other_handle.clone();
    let commit_pool = pool.clone();
    let (commit_tx, commit_rx) = mpsc::channel();
    let commit_task = runtime.spawn(async move {
        let committed = commit_pool
            .try_commit_connection("other".into(), commit_handle, McpServiceWrapper::Closed)
            .is_ok();
        let _ = commit_tx.send(committed);
    });
    let before_release = commit_rx.recv_timeout(LOCK_WAIT);
    let committed_before_release = before_release.is_ok();
    // 即使旧实现阻塞提交，也先解除停点再等待两个任务，避免 runtime Drop 卡死。
    drop(retention_latch);
    let committed = match before_release {
        Ok(committed) => Some(committed),
        Err(_) => commit_rx.recv_timeout(LOCK_WAIT).ok(),
    };
    let control_result = control_rx.recv_timeout(Duration::from_secs(20));
    let current = pool.get_client("controlled");
    let unrelated = pool.get_client("other");
    let old_gate_closed = !old_handle.call_gate.is_open();
    let current_gate_open = current.as_ref().is_some_and(|h| h.call_gate.is_open());
    let joined = runtime.block_on(async {
        peri_time::timeout(LOCK_WAIT, async { tokio::join!(control_task, commit_task) }).await
    });
    let cleanup = runtime.block_on(async {
        peri_time::timeout(Duration::from_secs(20), async {
            pool.begin_shutdown();
            let tasks = owner.shutdown().await;
            (tasks, pool.shutdown().await)
        })
        .await
    });
    runtime.shutdown_timeout(LOCK_WAIT);
    assert!(handoff_reached, "控制任务须到达已登记 owner 的确定停点");
    assert!(
        committed_before_release,
        "{action} 暂停交接时不得阻塞另一 server 的提交: {before_release:?}"
    );
    assert_eq!(committed, Some(true), "另一 server 的提交须被准入");
    assert!(Arc::ptr_eq(&unrelated.unwrap(), &other_handle));
    assert!(old_gate_closed, "控制须先撤销旧连接");
    let response = control_result
        .expect("释放停点后控制须有界完成")
        .expect("控制应完成");
    let current = current.unwrap();
    if action == "stop" {
        assert_eq!(response.status, McpControlStatus::Stopped);
        assert_eq!(current.status, ClientStatus::Disconnected);
    } else {
        assert_eq!(response.status, McpControlStatus::Connected);
        assert_eq!(current.status, ClientStatus::Connected);
        assert!(!Arc::ptr_eq(&current, &old_handle), "重启须发布新代");
        assert!(current.peer.is_some());
        assert!(current_gate_open);
    }
    let (control_join, commit_join) = joined.expect("释放停点后两个任务须有界 join");
    control_join.expect("控制任务不得 panic");
    commit_join.expect("提交任务不得 panic");
    let (tasks, services) = cleanup.expect("测试资源须有界收口");
    assert_eq!(
        tasks,
        super::super::super::task_scope::McpTaskShutdownReport::Complete
    );
    assert!(services.is_complete(), "{services:?}");
}

/// [回归测试] stop 的 service 交接曾持 services 锁再取 lifecycle，
/// 与另一 server 的 lifecycle → services 提交顺序相反。
#[test]
fn stop_handoff_allows_another_server_to_commit_before_retention_resumes() {
    control_and_commit_at_service_handoff("stop");
}

/// [回归测试] restart 共用旧代关闭路径；另一 server 的提交不应被旧代交接阻塞，
/// 释放停点后仍须完成真实 stdio 握手并保留新代清理 owner。
#[test]
fn restart_handoff_allows_another_server_to_commit_before_retention_resumes() {
    control_and_commit_at_service_handoff("restart");
}
