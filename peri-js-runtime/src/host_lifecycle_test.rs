use super::*;
use std::path::PathBuf;

/// The leader exits while its same-group descendant retains only stdout.
/// Drop is a panic fallback; normal assertions happen after explicit cleanup.
struct ExitedLeader {
    host: JsExecutionHost,
    directory: tempfile::TempDir,
    group: i32,
    descendant: i32,
}

impl ExitedLeader {
    async fn spawn() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let release = directory.path().join("release");
        let descendant_script = format!(
            r#"const fs = require('node:fs');
fs.writeFileSync({}, String(process.pid));
setInterval(() => {{
  if (fs.existsSync({})) {{
    process.stdout.write('{{"jsonrpc":"2.0","method":"fixture/released"}}\n', () => process.exit(0));
  }}
}}, 5);
setTimeout(() => process.exit(98), 15000);"#,
            serde_json::to_string(&ready).unwrap(),
            serde_json::to_string(&release).unwrap(),
        );
        let leader_script = format!(
            r#"const fs = require('node:fs');
const child = require('node:child_process').spawn(process.execPath, ['-e', {}], {{stdio: ['ignore', 'inherit', 'ignore']}});
child.on('error', () => process.exit(97));
setInterval(() => {{ if (fs.existsSync({})) process.exit(0); }}, 5);
setTimeout(() => process.exit(96), 15000);"#,
            serde_json::to_string(&descendant_script).unwrap(),
            serde_json::to_string(&ready).unwrap(),
        );
        let home = directory.path().to_string_lossy().into_owned();
        let spec = JsProcessSpec::new("node", vec!["-e".into(), leader_script])
            .without_inherited_environment()
            .with_environment([
                ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
                ("HOME".into(), home.clone()),
                ("XDG_CACHE_HOME".into(), home.clone()),
                ("npm_config_cache".into(), home.clone()),
                ("TMPDIR".into(), home.clone()),
            ])
            .with_cwd(home);
        let host = JsExecutionHost::spawn(spec).expect("local Node fixture must spawn");
        let group = host.child.lock().await.id().unwrap() as i32;
        let mut fixture = Self {
            host,
            directory,
            group,
            descendant: 0,
        };
        fixture.descendant = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(&ready).await {
                    if let Ok(pid) = value.parse::<i32>() {
                        if pid > 0 {
                            break pid;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("descendant must write READY before lifecycle assertion");
        let status = tokio::time::timeout(Duration::from_secs(3), fixture.host.wait_for_exit())
            .await
            .expect("leader must exit before lifecycle assertion")
            .unwrap();
        assert!(status.success(), "fixture leader failed: {status}");
        // SAFETY: this queries the positive PID written by this fixture's child.
        assert_eq!(unsafe { libc::getpgid(fixture.descendant) }, group);
        fixture
    }

    fn release_path(&self) -> PathBuf {
        self.directory.path().join("release")
    }

    fn kill_group(&self) {
        // SAFETY: spawn creates a dedicated positive process group owned by this fixture.
        unsafe { libc::kill(-self.group, libc::SIGKILL) };
    }

    async fn cleanup(&self) {
        self.kill_group();
        let _ = self.host.child.lock().await.wait().await;
        for slot in [&self.host.stdout_task, &self.host.stderr_task] {
            if let Some(task) = slot.lock().await.take() {
                task.abort();
                let _ = task.await;
            }
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while process_running(self.descendant) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture cleanup must stop the descendant even after a failed assertion");
    }
}

impl Drop for ExitedLeader {
    fn drop(&mut self) {
        self.kill_group();
    }
}

fn process_running(pid: i32) -> bool {
    // SAFETY: signal 0 only probes the fixture's recorded positive PID.
    if unsafe { libc::kill(pid, 0) } == -1 {
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    }
    // A container's init may defer reaping orphan zombies. They no longer execute or
    // retain stdout; the host can reap its direct child, not an adopted grandchild.
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        if stat
            .rsplit_once(") ")
            .is_some_and(|(_, tail)| tail.starts_with('Z'))
        {
            return false;
        }
    }
    true
}

#[tokio::test]
async fn test_exited_leader_cleanup_terminates_descendant() {
    let fixture = ExitedLeader::spawn().await;
    let cleanup = tokio::time::timeout(Duration::from_millis(250), fixture.host.kill()).await;
    let descendant_stopped = tokio::time::timeout(Duration::from_secs(1), async {
        while process_running(fixture.descendant) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    fixture.cleanup().await;

    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "kill must terminate the remaining tree before waiting for stdout EOF: {cleanup:?}"
    );
    assert!(
        descendant_stopped.is_ok(),
        "successful cleanup must leave no executing descendant"
    );
}

#[tokio::test]
async fn test_cancelled_reader_join_retains_owner() {
    let fixture = ExitedLeader::spawn().await;
    let mut incoming = fixture.host.take_incoming().await.unwrap();
    let first = tokio::time::timeout(Duration::from_millis(50), fixture.host.wait()).await;
    let retry = tokio::time::timeout(Duration::from_millis(50), fixture.host.wait()).await;
    tokio::fs::write(fixture.release_path(), b"release")
        .await
        .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(3), fixture.host.wait()).await;
    // Consume the real descendant's final frame and observe reader EOF, even on the
    // old implementation where the first wait detached its stdout JoinHandle.
    let reader_eof = tokio::time::timeout(Duration::from_secs(3), async {
        let mut saw_frame = false;
        while let Some(message) = incoming.recv().await {
            if matches!(message, IncomingMessage::Request { id: None, ref method, .. } if method == "fixture/released") {
                saw_frame = true;
            }
        }
        saw_frame
    })
    .await;
    fixture.cleanup().await;

    assert!(first.is_err(), "stdout must remain open before RELEASE");
    assert!(
        retry.is_err(),
        "cancelled wait must retain the reader join owner"
    );
    assert!(matches!(completed, Ok(Ok(status)) if status.success()));
    assert!(
        matches!(reader_eof, Ok(true)),
        "the real reader must drain the final frame and reach EOF"
    );
}
