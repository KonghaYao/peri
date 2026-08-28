//! Tests for mod_attrib

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::*;

const FIXTURE_READY_ENV: &str = "PERI_ATTRIBUTION_FIXTURE_READY";
const FIXTURE_RELEASE_ENV: &str = "PERI_ATTRIBUTION_FIXTURE_RELEASE";
const FIXTURE_SENTINEL_ENV: &str = "PERI_ATTRIBUTION_FIXTURE_SENTINEL";

#[cfg(unix)]
fn successful_branch_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "printf '  feature/windows\\n'"]);
    command
}

#[cfg(windows)]
fn successful_branch_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("cmd.exe");
    command.args(["/D", "/C", "echo   feature/windows  "]);
    command
}

async fn wait_for_fixture_ready(child: &mut tokio::process::Child, ready: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ready.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("应能查询 fixture 子进程状态") {
            panic!("fixture 在 READY 前意外退出：{status}");
        }
        assert!(Instant::now() < deadline, "fixture 未在期限内写入 READY");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn assert_sentinel_remains_absent(sentinel: &Path) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        assert!(
            !sentinel.exists(),
            "超时后的 fixture 仍存活并写入了 SENTINEL"
        );
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn test_git_attribution_reset_clears_pending() {
    let mw = GitAttributionMiddleware::new("test-model");
    // 插入一些待处理内容
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file1.rs".to_string(), "old content".to_string());
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file2.rs".to_string(), "more content".to_string());
    assert_eq!(mw.pending_old_content.lock().unwrap().len(), 2);

    // reset 后应清空
    mw.reset();
    assert!(mw.pending_old_content.lock().unwrap().is_empty());
}

#[test]
fn test_branch_drift_reports_each_change_once() {
    let mw = GitAttributionMiddleware::new("test-model");

    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(
        mw.observe_branch("feature".to_string()),
        Some(("main".to_string(), "feature".to_string()))
    );
    assert_eq!(mw.observe_branch("feature".to_string()), None);
}

#[tokio::test]
async fn test_current_branch_with_command_trims_successful_output() {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        GitAttributionMiddleware::current_branch_with_command(
            successful_branch_command(),
            Duration::from_secs(1),
        ),
    )
    .await
    .expect("分支命令应在外层测试期限内完成");
    assert_eq!(result, Some("feature/windows".to_string()));
}

#[test]
#[ignore]
fn current_branch_hanging_process_fixture() {
    let Some(ready) = std::env::var_os(FIXTURE_READY_ENV).map(PathBuf::from) else {
        return;
    };
    let Some(release) = std::env::var_os(FIXTURE_RELEASE_ENV).map(PathBuf::from) else {
        return;
    };
    let Some(sentinel) = std::env::var_os(FIXTURE_SENTINEL_ENV).map(PathBuf::from) else {
        return;
    };

    std::fs::write(&ready, b"ready").expect("fixture 应能写入 READY");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if release.exists() {
            std::fs::write(&sentinel, b"escaped").expect("存活的 fixture 应能写入 SENTINEL");
            return;
        }
        if Instant::now() >= deadline {
            std::fs::write(&sentinel, b"guard-expired")
                .expect("fixture guard 到期时应写入 SENTINEL");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// [回归测试] Git 分支探测超时后必须终止直接子进程，不能让它脱离等待继续运行。
///
/// 历史背景：Windows ACP 的 `git rev-parse` 曾永久不退出，attribution 在同步
/// `before_agent` 路径无限等待；仅给 future 加 timeout 会留下仍在运行的子进程。
#[tokio::test]
async fn test_current_branch_with_command_timeout_kills_process() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let temp_dir = tempfile::tempdir().expect("应能创建 fixture 临时目录");
        let ready = temp_dir.path().join("ready");
        let release = temp_dir.path().join("release");
        let sentinel = temp_dir.path().join("sentinel");
        let current_exe = std::env::current_exe().expect("应能定位当前测试可执行文件");
        let mut command = tokio::process::Command::new(current_exe);
        command
            .args([
                "--exact",
                "--ignored",
                "attribution::tests::current_branch_hanging_process_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(FIXTURE_READY_ENV, &ready)
            .env(FIXTURE_RELEASE_ENV, &release)
            .env(FIXTURE_SENTINEL_ENV, &sentinel);
        let mut child = GitAttributionMiddleware::spawn_branch_command(command)
            .expect("应能启动 fixture 子进程");
        wait_for_fixture_ready(&mut child, &ready).await;

        let result =
            GitAttributionMiddleware::current_branch_from_child(child, Duration::from_millis(100))
                .await;
        assert_eq!(result, None);
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(&release, b"release").expect("应能写入 RELEASE");
        assert_sentinel_remains_absent(&sentinel).await;
        assert!(!sentinel.exists(), "fixture 不应在测试结束前写入 SENTINEL");
    })
    .await
    .expect("回归测试生命周期应在外层期限内完成");
}
