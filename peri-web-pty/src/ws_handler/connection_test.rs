use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

struct ThreadWatchdog {
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    expired: Arc<AtomicBool>,
}
impl ThreadWatchdog {
    fn new(home: std::path::PathBuf) -> Self {
        let (stop, receiver) = mpsc::channel();
        let expired = Arc::new(AtomicBool::new(false));
        let flag = expired.clone();
        let worker = std::thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(Duration::from_secs(3)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                flag.store(true, Ordering::SeqCst);
                for name in ["leader", "holder"] {
                    if let Ok(pid) = std::fs::read_to_string(home.join(name)) {
                        if let Ok(pid) = pid.parse::<i32>() {
                            if pid > 0 {
                                // SAFETY: these are the pids recorded by this isolated fixture.
                                unsafe {
                                    libc::kill(pid, libc::SIGKILL);
                                }
                            }
                        }
                    }
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
            expired,
        }
    }
}
impl Drop for ThreadWatchdog {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let _ = self.worker.take().unwrap().join();
    }
}

struct SlaveHolder(tempfile::TempDir);
impl Drop for SlaveHolder {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.path().join("holder")) {
            if let Ok(pid) = pid.parse::<i32>() {
                if pid > 0 {
                    // SAFETY: this is the deliberately surviving fixture process.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn shutdown_cancels_idle_read_and_reaps_child_without_slave_eof() {
    let fixture = SlaveHolder(tempfile::tempdir().unwrap());
    let (session, reader) = PtySession::spawn(
        "/bin/sh",
        &["-c", "(trap '' HUP; touch holder-ready; exec sleep 30) & while [ ! -e holder-ready ]; do sleep 0.01; done; printf '%s' \"$!\" > holder; printf '%s' \"$$\" > leader; printf READY; wait"],
        80, 24, fixture.0.path().to_str(),
    ).unwrap();
    let watchdog = ThreadWatchdog::new(fixture.0.path().to_owned());
    let started = std::time::Instant::now();
    let mut owner = Connection::new(session, reader).await.unwrap();
    let mut buffer = [0u8; 4096];
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut output = String::new();
        while !output.contains("READY") {
            let count = owner.io.as_ref().unwrap().read(&mut buffer).await.unwrap();
            assert!(count > 0);
            output.push_str(&String::from_utf8_lossy(&buffer[..count]));
        }
    })
    .await
    .unwrap();
    let leader = std::fs::read_to_string(fixture.0.path().join("leader"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let holder = std::fs::read_to_string(fixture.0.path().join("holder"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(leader > 0 && holder > 0);
    // The slave is open but quiet: readiness must be cancellable without a blocked worker.
    assert!(tokio::time::timeout(
        Duration::from_millis(20),
        owner.io.as_ref().unwrap().read(&mut buffer)
    )
    .await
    .is_err());
    tokio::time::timeout(Duration::from_secs(2), owner.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(owner.io.is_none() && owner.session.is_none());
    assert!(
        !watchdog.expired.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(3),
        "cleanup must precede the independent watchdog"
    );
    // SAFETY: signal zero probes the two pids produced by this isolated fixture.
    assert_eq!(
        unsafe { libc::kill(leader, 0) },
        -1,
        "shutdown must reap the child"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    // SAFETY: the fixture guard owns this holder and terminates it after this assertion.
    assert_eq!(
        unsafe { libc::kill(holder, 0) },
        0,
        "cleanup must not require all slave holders to exit"
    );
}

#[test]
fn interrupted_shutdown_retains_the_join_until_a_retry_completes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (release, blocked) = mpsc::channel();
    let (started, observed) = mpsc::channel();
    let _blocker = runtime.spawn_blocking(move || {
        started.send(()).unwrap();
        let _ = blocked.recv();
    });
    observed.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.block_on(async {
        let fixture = tempfile::tempdir().unwrap();
        let (session, reader) = PtySession::spawn(
            "/bin/sh",
            &[
                "-c",
                "printf '%s' \"$$\" > leader; printf READY; exec sleep 30",
            ],
            80,
            24,
            fixture.path().to_str(),
        )
        .unwrap();
        let watchdog = ThreadWatchdog::new(fixture.path().to_owned());
        let mut owner = Connection::new(session, reader).await.unwrap();
        let mut buffer = [0; 4096];
        assert!(owner.io.as_ref().unwrap().read(&mut buffer).await.unwrap() > 0);
        let pid = std::fs::read_to_string(fixture.path().join("leader"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert!(pid > 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), owner.shutdown())
                .await
                .is_err()
        );
        assert!(
            owner.cleanup.is_some(),
            "cancelled await must retain the original cleanup handle"
        );
        // SAFETY: the occupied blocking pool guarantees that cleanup has not run yet.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);

        release.send(()).unwrap();
        owner.shutdown().await.unwrap();
        assert!(owner.cleanup.is_none());
        assert!(!watchdog.expired.load(Ordering::SeqCst));
        // SAFETY: the completed retry must have waited for this fixture-owned child.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    });
    drop(release);
}
