use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use peri_acp_types::{dynamic_mcp::DynamicMcpShutdownReport, ports::McpPoolShutdownReport};
use tokio::sync::{oneshot, Barrier};

use super::{
    Admission, DrainFuture, HostShutdownReport, HostTaskKind, HostTaskOwner, HostTaskOwnerKind,
    HostTerminalShutdownReport, HostWaitDriver, WaitFuture, WaitOutcome, WaitPhase,
};

#[test]
fn test_terminal_shutdown_evidence_requires_all_subsystems_and_sessions_complete() {
    let complete_pool = McpPoolShutdownReport::Complete {
        settled_services: 2,
        failed_services: 0,
    };
    assert!(matches!(
        HostTerminalShutdownReport::aggregate(
            HostShutdownReport::Complete,
            DynamicMcpShutdownReport::Complete,
            complete_pool,
            0,
        ),
        HostTerminalShutdownReport::Complete { .. }
    ));

    for report in [
        HostTerminalShutdownReport::aggregate(
            HostShutdownReport::Incomplete { unfinished: 1 },
            DynamicMcpShutdownReport::Complete,
            complete_pool,
            0,
        ),
        HostTerminalShutdownReport::aggregate(
            HostShutdownReport::Complete,
            DynamicMcpShutdownReport::Incomplete {
                unfinished_instances: 1,
            },
            complete_pool,
            0,
        ),
        HostTerminalShutdownReport::aggregate(
            HostShutdownReport::Complete,
            DynamicMcpShutdownReport::Complete,
            McpPoolShutdownReport::Incomplete {
                settled_services: 1,
                unfinished_services: 1,
                failed_services: 0,
            },
            0,
        ),
        HostTerminalShutdownReport::aggregate(
            HostShutdownReport::Complete,
            DynamicMcpShutdownReport::Complete,
            complete_pool,
            1,
        ),
    ] {
        assert!(matches!(
            report,
            HostTerminalShutdownReport::Incomplete { .. }
        ));
    }
}

struct ControlledWaitDriver {
    expiries: Mutex<VecDeque<(WaitPhase, oneshot::Receiver<()>)>>,
}

impl HostWaitDriver for ControlledWaitDriver {
    fn race<'a>(
        &'a self,
        phase: WaitPhase,
        _duration: Duration,
        drain: DrainFuture<'a>,
    ) -> WaitFuture<'a> {
        let expiry = self
            .expiries
            .lock()
            .expect("controlled wait queue poisoned")
            .pop_front();
        let Some((expected_phase, expiry)) = expiry else {
            return Box::pin(async move {
                drain.await;
                WaitOutcome::Drained
            });
        };
        assert_eq!(phase, expected_phase);
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = expiry => WaitOutcome::Expired,
                () = drain => WaitOutcome::Drained,
            }
        })
    }
}

fn controlled_driver(
    phases: &[WaitPhase],
) -> (Arc<ControlledWaitDriver>, Vec<oneshot::Sender<()>>) {
    let mut expiries = VecDeque::new();
    let mut triggers = Vec::new();
    for phase in phases {
        let (tx, rx) = oneshot::channel();
        expiries.push_back((*phase, rx));
        triggers.push(tx);
    }
    (
        Arc::new(ControlledWaitDriver {
            expiries: Mutex::new(expiries),
        }),
        triggers,
    )
}

#[tokio::test]
async fn test_scope_rejects_spawn_after_begin_shutdown() {
    let (mut owner, spawner) = HostTaskOwner::new();
    let (release_tx, release_rx) = oneshot::channel();
    spawner
        .spawn(HostTaskOwnerKind::Host, HostTaskKind::Prompt, async move {
            let _ = release_rx.await;
        })
        .unwrap();
    owner.begin_shutdown();
    let started = Arc::new(AtomicBool::new(false));
    let started_task = started.clone();
    assert!(spawner
        .spawn(
            HostTaskOwnerKind::Host,
            HostTaskKind::Prediction,
            async move {
                started_task.store(true, Ordering::SeqCst);
            }
        )
        .is_err());
    release_tx.send(()).unwrap();
    assert_eq!(owner.shutdown().await, HostShutdownReport::Complete);
    assert!(!started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_scope_zero_grace_aborts_and_drains_pending_task() {
    let (mut owner, spawner) = HostTaskOwner::with_policy(Duration::ZERO, Duration::from_secs(1));
    let dropped = Arc::new(AtomicBool::new(false));
    struct Probe(Arc<AtomicBool>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let probe = Probe(dropped.clone());
    spawner
        .spawn(HostTaskOwnerKind::Host, HostTaskKind::Prompt, async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        })
        .unwrap();
    assert_eq!(owner.shutdown().await, HostShutdownReport::Complete);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_scope_shutdown_is_idempotent() {
    let (mut owner, _spawner) = HostTaskOwner::new();
    assert_eq!(owner.shutdown().await, HostShutdownReport::Complete);
    assert_eq!(owner.shutdown().await, HostShutdownReport::Complete);
}

#[tokio::test]
async fn test_scope_does_not_hold_registry_lock_while_waiting() {
    let (mut owner, spawner) = HostTaskOwner::new();
    let (release_tx, release_rx) = oneshot::channel();
    spawner
        .spawn(HostTaskOwnerKind::Host, HostTaskKind::Prompt, async move {
            let _ = release_rx.await;
        })
        .unwrap();
    let shutdown = tokio::spawn(async move { owner.shutdown().await });
    tokio::task::yield_now().await;

    assert_eq!(spawner.snapshot(), Some((Admission::Closing, 1)));
    release_tx.send(()).unwrap();
    assert_eq!(shutdown.await.unwrap(), HostShutdownReport::Complete);
}

#[tokio::test]
async fn test_zero_work_spawn_racing_shutdown_is_rejected_or_registered() {
    let (owner, spawner) = HostTaskOwner::new();
    let owner = Arc::new(Mutex::new(owner));
    let gate = Arc::new(Barrier::new(2));
    let spawn_gate = gate.clone();
    let spawn_spawner = spawner.clone();
    let spawn = tokio::spawn(async move {
        spawn_gate.wait().await;
        spawn_spawner.spawn(HostTaskOwnerKind::Host, HostTaskKind::Prediction, async {})
    });
    gate.wait().await;
    owner.lock().expect("owner mutex poisoned").begin_shutdown();
    let result = spawn.await.unwrap();
    if result.is_ok() {
        assert!(spawner.snapshot().is_some_and(|(_, count)| count >= 1));
    } else {
        assert_eq!(
            spawner.snapshot().map(|(phase, _)| phase),
            Some(Admission::Closing)
        );
    }
    drop(owner);
}

#[tokio::test]
async fn test_dropping_owner_before_server_start_aborts_assembly_task() {
    struct ConfigLike {
        _owner: Option<HostTaskOwner>,
    }
    struct Probe(Option<oneshot::Sender<()>>);
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let (owner, spawner) = HostTaskOwner::new();
    let config = ConfigLike {
        _owner: Some(owner),
    };
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let probe = Probe(Some(dropped_tx));
    spawner
        .spawn(
            HostTaskOwnerKind::Startup,
            HostTaskKind::PluginCleanup,
            async move {
                let _probe = probe;
                std::future::pending::<()>().await;
            },
        )
        .unwrap();

    drop(config);
    dropped_rx
        .await
        .expect("owner Drop must request task abort");
    assert!(spawner
        .spawn(
            HostTaskOwnerKind::Startup,
            HostTaskKind::PluginCleanup,
            async {}
        )
        .is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn test_incomplete_abort_drain_is_reported_not_claimed_complete() {
    let (driver, triggers) = controlled_driver(&[WaitPhase::Cooperative, WaitPhase::AbortDrain]);
    let (mut owner, spawner) =
        HostTaskOwner::with_wait_driver(Duration::from_secs(60), Duration::from_secs(60), driver);
    let dropped = Arc::new(AtomicBool::new(false));
    struct Probe(Arc<AtomicBool>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let probe = Probe(dropped.clone());
    spawner
        .spawn(HostTaskOwnerKind::Host, HostTaskKind::Prompt, async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        })
        .unwrap();
    for trigger in triggers {
        trigger.send(()).unwrap();
    }

    assert_eq!(
        owner.shutdown().await,
        HostShutdownReport::Incomplete { unfinished: 1 }
    );
    assert_eq!(
        spawner.snapshot().map(|(phase, _)| phase),
        Some(Admission::Closing)
    );
    assert!(!dropped.load(Ordering::SeqCst));
    assert_eq!(owner.shutdown().await, HostShutdownReport::Complete);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(spawner.snapshot(), Some((Admission::Closed, 0)));
}

#[tokio::test]
async fn test_spawner_is_non_owning() {
    let (owner, spawner) = HostTaskOwner::new();
    drop(owner);
    assert!(spawner
        .spawn(
            HostTaskOwnerKind::Startup,
            HostTaskKind::PluginCleanup,
            async {}
        )
        .is_err());
}
