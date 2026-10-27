use super::*;
use std::{path::Path, path::PathBuf, time::Duration};

// 启动两个真实 Node 进程不属于回收时限断言。Linux Node 22 容器限制为
// 0.05 CPU 的实验中，三组并发握手耗时 3.8–5.2s；这并非 CI 负载的测量值。
// 仅启动留出余量，下面的生命周期时限不变。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The leader exits while its same-group descendant retains only stdout.
/// Drop is a panic fallback; normal assertions happen after explicit cleanup.
struct ExitedLeader {
    host: JsExecutionHost,
    directory: tempfile::TempDir,
    group: i32,
    descendant: i32,
    cleaned: bool,
}

impl ExitedLeader {
    async fn spawn() -> Self {
        Self::spawn_with_stdout("inherit").await
    }

    async fn spawn_with_stdout(stdout: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let release = directory.path().join("release");
        let descendant_script = format!(
            r#"const fs = require('node:fs');
const ready = {};
fs.writeFileSync(ready + '.tmp', String(process.pid));
fs.renameSync(ready + '.tmp', ready);
setInterval(() => {{
  if (fs.existsSync({})) {{
    process.stdout.write('{{"jsonrpc":"2.0","method":"fixture/released"}}\n', () => process.exit(0));
  }}
}}, 5);
setTimeout(() => process.exit(98), 30000);"#,
            serde_json::to_string(&ready).unwrap(),
            serde_json::to_string(&release).unwrap(),
        );
        let leader_script = Self::leader_script(&descendant_script, &ready, stdout);
        let mut fixture = Self::spawn_script(directory, leader_script).await;
        fixture.finish_startup(STARTUP_TIMEOUT).await.unwrap();
        fixture
    }

    fn leader_script(descendant_script: &str, ready: &Path, stdout: &str) -> String {
        // stderr 由 leader 转发而非后代继承，leader 退出后宿主仍能观察 stderr EOF；
        // 保留 stdout=ignore 用例中「两个 reader 均 EOF，但后代仍存活」的回归前提。
        format!(
            r#"const fs = require('node:fs');
const ready = {ready};
const child = require('node:child_process').spawn(process.execPath, ['-e', {script}], {{stdio: ['ignore', {stdout}, 'pipe']}});
child.stderr.on('data', data => process.stderr.write(data));
child.on('error', error => {{
  process.stderr.write('fixture descendant spawn failed: ' + error.message + '\n', () => process.exit(97));
}});
child.on('close', (code, signal) => {{
  if (!fs.existsSync(ready)) {{
    process.stderr.write('fixture descendant exited before READY: ' + code + ' ' + signal + '\n', () => process.exit(97));
  }}
}});
setInterval(() => {{ if (fs.existsSync(ready)) process.exit(0); }}, 5);
setTimeout(() => process.exit(96), 30000);"#,
            ready = serde_json::to_string(ready).unwrap(),
            script = serde_json::to_string(descendant_script).unwrap(),
            stdout = serde_json::to_string(stdout).unwrap(),
        )
    }

    async fn spawn_script(directory: tempfile::TempDir, leader_script: String) -> Self {
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
        Self {
            host,
            directory,
            group,
            descendant: 0,
            cleaned: false,
        }
    }

    async fn finish_startup(&mut self, budget: Duration) -> std::result::Result<(), String> {
        let result = tokio::time::timeout(budget, self.wait_for_ready_and_leader())
            .await
            .map_err(|_| format!("fixture startup timed out after {budget:?}"))
            .and_then(|result| result);
        if let Err(error) = result {
            // 只有本文件 env_clear 后运行的合成脚本可在测试错误中附带 stderr。
            // 先杀整组、reap leader 并排空 reader，再返回失败，避免 panic 遗留子进程。
            let cleanup = self.cleanup().await;
            return Err(format!(
                "{error}; cleanup: {cleanup:?}; isolated fixture stderr: {:?}",
                self.host.stderr_tail()
            ));
        }
        Ok(())
    }

    async fn wait_for_ready_and_leader(&mut self) -> std::result::Result<(), String> {
        let ready = self.directory.path().join("ready");
        loop {
            // 先观察退出状态再读 READY，避免文件刚发布、leader 刚退出时误报。
            let status = self
                .host
                .child
                .lock()
                .await
                .try_wait()
                .map_err(|error| format!("fixture leader status failed: {error}"))?;
            match tokio::fs::read_to_string(&ready).await {
                Ok(value) => {
                    self.descendant = value
                        .parse::<i32>()
                        .ok()
                        .filter(|pid| *pid > 0)
                        .ok_or_else(|| "fixture READY must contain a positive PID".to_string())?;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("fixture READY read failed: {error}")),
            }
            if let Some(status) = status {
                return Err(format!("fixture leader exited before READY: {status}"));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let status = self
            .host
            .wait_for_exit()
            .await
            .map_err(|error| format!("fixture leader wait failed: {error}"))?;
        if !status.success() {
            return Err(format!("fixture leader failed: {status}"));
        }
        // SAFETY: this queries the positive PID written by this fixture's child.
        if unsafe { libc::getpgid(self.descendant) } != self.group {
            return Err("fixture descendant left its owned process group".into());
        }
        Ok(())
    }

    fn release_path(&self) -> PathBuf {
        self.directory.path().join("release")
    }

    async fn cleanup(&mut self) -> std::result::Result<(), String> {
        if self.cleaned {
            return Ok(());
        }
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            // 复用 owner 的 settled 防护：已确认退出后，不再信号可能复用的 PGID。
            self.host.process_tree.terminate();
            self.host
                .child
                .lock()
                .await
                .wait()
                .await
                .map_err(|error| format!("fixture leader reap failed: {error}"))?;
            // READY 可能从未发布，不能仅靠已记录 PID 或 reader EOF 证明后代停止。
            self.host.process_tree.wait_for_exit().await;
            self.host.join_readers().await;
            while self.descendant > 0 && process_running(self.descendant) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(())
        })
        .await
        .map_err(|_| "fixture cleanup timed out".to_string())
        .and_then(|result| result);
        self.cleaned = result.is_ok();
        if result.is_err() {
            // 清理失败仍须报告；终止 reader 避免失败路径遗留后台任务。
            for slot in [&self.host.stdout_task, &self.host.stderr_task] {
                if let Some(task) = slot.lock().await.take() {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        result
    }
}

/// [回归测试] stdout EOF 不能证明已重定向输出的后代停止执行。
#[tokio::test]
async fn test_wait_retains_redirected_descendant_until_natural_exit() {
    let mut fixture = ExitedLeader::spawn_with_stdout("ignore").await;
    let readers = tokio::time::timeout(Duration::from_secs(3), fixture.host.join_readers()).await;
    if let Err(error) = readers {
        let cleanup = fixture.cleanup().await;
        panic!("重定向后代存活时 stdout/stderr 都必须已到 EOF：{error}; cleanup: {cleanup:?}");
    }
    let before_release = tokio::time::timeout(Duration::from_millis(50), fixture.host.wait()).await;
    tokio::fs::write(fixture.release_path(), b"release")
        .await
        .unwrap();
    let after_release = tokio::time::timeout(Duration::from_secs(3), fixture.host.wait()).await;
    let stopped = !process_running(fixture.descendant);
    fixture.cleanup().await.unwrap();
    assert!(before_release.is_err(), "无输出后代存活时 wait 不能成功");
    assert!(matches!(after_release, Ok(Ok(status)) if status.success()));
    assert!(stopped, "wait 成功返回前必须已确认整个进程树退出");
}

/// [回归测试] 取消回收 waiter 不得移走 child/tree/reader，重试仍能完成回收。
#[tokio::test]
async fn test_cancelled_kill_retries_same_owner_for_redirected_descendant() {
    let mut fixture = ExitedLeader::spawn_with_stdout("ignore").await;
    let child = fixture.host.child.lock().await;
    let interrupted = tokio::time::timeout(Duration::from_millis(50), fixture.host.kill()).await;
    drop(child);
    let retried = tokio::time::timeout(Duration::from_secs(3), fixture.host.kill()).await;
    let stopped = !process_running(fixture.descendant);
    fixture.cleanup().await.unwrap();
    assert!(
        interrupted.is_err(),
        "child owner 未释放前回收应保持 pending"
    );
    assert!(matches!(retried, Ok(Ok(()))), "重试必须排空原进程树");
    assert!(stopped, "kill 成功返回前后代必须已停止");
}

impl Drop for ExitedLeader {
    fn drop(&mut self) {
        if !self.cleaned {
            self.host.process_tree.terminate();
        }
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

async fn assert_fixture_cleaned(fixture: &ExitedLeader) {
    assert!(fixture.cleaned, "失败返回前必须完成显式清理");
    assert!(fixture.host.process_tree.is_stopped(), "必须确认整组退出");
    assert!(fixture
        .host
        .child
        .lock()
        .await
        .try_wait()
        .unwrap()
        .is_some());
    assert!(fixture.host.stdout_task.lock().await.is_none());
    assert!(fixture.host.stderr_task.lock().await.is_none());
}

#[tokio::test]
async fn test_fixture_accepts_ready_after_old_startup_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    // 真实 Node 延迟发布 READY，复现可工作的启动超过旧 3s deadline；
    // 不断言精确耗时，整组仍须在独立启动预算内成功，并显式回收。
    let descendant_script = format!(
        r#"const fs = require('node:fs');
const ready = {};
setTimeout(() => {{
  fs.writeFileSync(ready + '.tmp', String(process.pid));
  fs.renameSync(ready + '.tmp', ready);
}}, 3200);
setTimeout(() => process.exit(98), 30000);"#,
        serde_json::to_string(&ready).unwrap(),
    );
    let script = ExitedLeader::leader_script(&descendant_script, &ready, "ignore");
    let mut fixture = ExitedLeader::spawn_script(directory, script).await;
    let result = fixture.finish_startup(STARTUP_TIMEOUT).await;
    fixture.cleanup().await.unwrap();
    assert!(result.is_ok(), "延迟 READY 不应误报启动失败：{result:?}");
    assert_fixture_cleaned(&fixture).await;
}

#[tokio::test]
async fn test_fixture_reports_leader_exit_before_ready() {
    let directory = tempfile::tempdir().unwrap();
    let mut fixture = ExitedLeader::spawn_script(
        directory,
        "process.stderr.write('synthetic leader failure\\n', () => process.exit(41));".into(),
    )
    .await;
    let error = fixture.finish_startup(STARTUP_TIMEOUT).await.unwrap_err();
    assert!(error.contains("leader exited before READY"), "{error}");
    assert!(error.contains("41"), "{error}");
    assert!(error.contains("synthetic leader failure"), "{error}");
    assert_fixture_cleaned(&fixture).await;
}

#[tokio::test]
async fn test_fixture_reports_descendant_exit_and_stderr_before_ready() {
    let directory = tempfile::tempdir().unwrap();
    let script = ExitedLeader::leader_script(
        "process.stderr.write('synthetic descendant failure\\n', () => process.exit(43));",
        &directory.path().join("ready"),
        "ignore",
    );
    let mut fixture = ExitedLeader::spawn_script(directory, script).await;
    let error = fixture.finish_startup(STARTUP_TIMEOUT).await.unwrap_err();
    assert!(error.contains("leader exited before READY"), "{error}");
    assert!(
        error.contains("descendant exited before READY: 43"),
        "{error}"
    );
    assert!(error.contains("synthetic descendant failure"), "{error}");
    assert_fixture_cleaned(&fixture).await;
}

#[tokio::test]
async fn test_fixture_ready_timeout_kills_unannounced_descendant() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    let marker = "synthetic descendant waiting for READY";
    let descendant_script = format!(
        r#"const fs = require('node:fs');
process.stderr.write({marker}, () => {{
  fs.writeFileSync({started}, String(process.pid));
}});
setTimeout(() => process.exit(98), 30000);"#,
        marker = serde_json::to_string(&format!("{marker}\n")).unwrap(),
        started = serde_json::to_string(&started).unwrap(),
    );
    let script = ExitedLeader::leader_script(
        &descendant_script,
        &directory.path().join("ready"),
        "ignore",
    );
    let mut fixture = ExitedLeader::spawn_script(directory, script).await;
    let observed = tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            let pid = tokio::fs::read_to_string(&started)
                .await
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|pid| *pid > 0);
            // started 仅证明写入了 leader pipe；宿主实际收到标记后才开始短超时。
            let captured = fixture
                .host
                .stderr_tail()
                .is_some_and(|tail| tail.contains(marker));
            if let (Some(pid), true) = (pid, captured) {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let descendant = match observed {
        Ok(pid) => pid,
        Err(error) => {
            let cleanup = fixture.cleanup().await;
            panic!(
                "未收到 started PID 与宿主 stderr 标记：{error}; cleanup: {cleanup:?}; isolated fixture stderr: {:?}",
                fixture.host.stderr_tail()
            );
        }
    };
    let error = fixture
        .finish_startup(Duration::from_millis(25))
        .await
        .unwrap_err();
    assert!(error.contains("startup timed out"), "{error}");
    assert!(error.contains(marker), "{error}");
    assert!(!process_running(descendant), "READY 未发布的后代也必须停止");
    assert_fixture_cleaned(&fixture).await;
    // 清理成功后不再向已经释放的 PGID 发信号。
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_fixture_rejects_invalid_ready_and_cleans_up() {
    let directory = tempfile::tempdir().unwrap();
    let script = format!(
        "require('node:fs').writeFileSync({}, '0'); setTimeout(() => process.exit(96), 30000);",
        serde_json::to_string(&directory.path().join("ready")).unwrap(),
    );
    let mut fixture = ExitedLeader::spawn_script(directory, script).await;
    let error = fixture.finish_startup(STARTUP_TIMEOUT).await.unwrap_err();
    assert!(
        error.contains("READY must contain a positive PID"),
        "{error}"
    );
    assert_fixture_cleaned(&fixture).await;
}

#[tokio::test]
async fn test_exited_leader_cleanup_terminates_descendant() {
    let mut fixture = ExitedLeader::spawn().await;
    let cleanup = tokio::time::timeout(Duration::from_millis(250), fixture.host.kill()).await;
    let descendant_stopped = tokio::time::timeout(Duration::from_secs(1), async {
        while process_running(fixture.descendant) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    fixture.cleanup().await.unwrap();

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
async fn test_cleanup_and_drop_preserve_other_group_after_tree_exit() {
    for explicit_cleanup in [true, false] {
        let mut fixture = ExitedLeader::spawn_with_stdout("ignore").await;
        let stopped = tokio::time::timeout(Duration::from_secs(3), fixture.host.kill()).await;
        if !matches!(&stopped, Ok(Ok(()))) {
            let cleanup = fixture.cleanup().await;
            panic!("前置进程树退出失败：{stopped:?}; cleanup: {cleanup:?}");
        }
        let mut other = ExitedLeader::spawn().await;
        let mut incoming = other.host.take_incoming().await.unwrap();
        // 仅替换夹具记录的 PGID，模拟旧编号已复用；目标也是本测试拥有的树。
        fixture.group = other.group;
        let cleanup = if explicit_cleanup {
            fixture.cleanup().await
        } else {
            Ok(())
        };
        drop(fixture);
        // 用新的 RELEASE/响应握手证明仍可执行，不以信号 0 的瞬时探测代替。
        let released = tokio::fs::write(other.release_path(), b"release").await;
        let response = tokio::time::timeout(Duration::from_secs(3), incoming.recv()).await;
        let other_cleanup = other.cleanup().await;
        assert!(cleanup.is_ok(), "原树清理失败：{cleanup:?}");
        assert!(released.is_ok(), "对照树 RELEASE 失败：{released:?}");
        assert!(
            matches!(response, Ok(Some(IncomingMessage::Request { id: None, ref method, .. })) if method == "fixture/released"),
            "已结束 owner 的 cleanup/Drop 不得信号其他树：explicit_cleanup={explicit_cleanup}"
        );
        assert!(other_cleanup.is_ok(), "对照树清理失败：{other_cleanup:?}");
    }
}

#[tokio::test]
async fn test_cancelled_reader_join_retains_owner() {
    let mut fixture = ExitedLeader::spawn().await;
    let mut incoming = fixture.host.take_incoming().await.unwrap();
    let first = tokio::time::timeout(Duration::from_millis(50), fixture.host.wait()).await;
    let retry = tokio::time::timeout(Duration::from_millis(50), fixture.host.wait()).await;
    // host.wait 尚停在进程树等待；直接进入 join 才能覆盖 reader 等待被取消。
    let join_first =
        tokio::time::timeout(Duration::from_millis(50), fixture.host.join_readers()).await;
    let first_owner_retained = fixture
        .host
        .stdout_task
        .lock()
        .await
        .as_ref()
        .is_some_and(|task| !task.is_finished());
    let join_retry =
        tokio::time::timeout(Duration::from_millis(50), fixture.host.join_readers()).await;
    let retry_owner_retained = fixture
        .host
        .stdout_task
        .lock()
        .await
        .as_ref()
        .is_some_and(|task| !task.is_finished());
    tokio::fs::write(fixture.release_path(), b"release")
        .await
        .unwrap();
    let join_completed =
        tokio::time::timeout(Duration::from_secs(3), fixture.host.join_readers()).await;
    let readers_drained = fixture.host.stdout_task.lock().await.is_none()
        && fixture.host.stderr_task.lock().await.is_none();
    let completed = tokio::time::timeout(Duration::from_secs(3), fixture.host.wait()).await;
    // 消费真实后代的最终帧并观察 EOF；不能把 JoinHandle 被移走误当成 join 完成。
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
    fixture.cleanup().await.unwrap();

    assert!(first.is_err(), "stdout must remain open before RELEASE");
    assert!(retry.is_err(), "取消 wait 后重试仍须等待整树退出");
    assert!(
        join_first.is_err(),
        "RELEASE 前 reader join 必须保持 pending"
    );
    assert!(first_owner_retained, "第一次取消 join 不得移走 handle");
    assert!(join_retry.is_err(), "取消 join 后重试仍须等待真实 EOF");
    assert!(retry_owner_retained, "再次取消 join 仍须保留同一个 owner");
    assert!(join_completed.is_ok(), "RELEASE 后直接重试 join 必须完成");
    assert!(readers_drained, "完成 join 后才能清空 reader handles");
    assert!(matches!(completed, Ok(Ok(status)) if status.success()));
    assert!(
        matches!(reader_eof, Ok(true)),
        "the real reader must drain the final frame and reach EOF"
    );
}
