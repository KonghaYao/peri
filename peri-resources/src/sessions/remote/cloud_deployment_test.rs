//! Explicit cloud deployment lifecycle. Each phase runs in a new process. Turso
//! stores every persistent fact remotely; no phase may create a local SQLite file.

use std::path::{Path, PathBuf};

use super::cloud_tests::{check, failure, run_counts, unique_run_label, with_cleanup, CloudTarget};
use super::mutation::StoreAccess;

pub(super) const HOME_ENV: &str = "PERI_CLOUD_FACADE_HOME";
pub(super) const WORKSPACE_ENV: &str = "PERI_CLOUD_FACADE_WORKSPACE";
pub(super) const RUN_ENV: &str = "PERI_CLOUD_FACADE_RUN";
pub(super) const READ_ONLY_ENV: &str = "PERI_CLOUD_FACADE_READ_ONLY";
pub(super) const ROOT_SUFFIX: &str = "root";
pub(super) const CHILD_SUFFIX: &str = "child";
pub(super) const FORK_SUFFIX: &str = "fork";

fn local_sqlite_path(home: &Path) -> PathBuf {
    home.join(".peri").join("threads").join("threads.db")
}

const WRITE_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_writes_the_synthetic_tree";
const RECOVER_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_recovers_cold_then_rewinds_and_deletes";
const READ_ONLY_CHILD: &str = "sessions::remote::cloud_deployment_child_tests::cloud_deployment_child_read_only_leaves_no_trace";

/// 端到端：真实部署入口在真引擎上的完整生命周期。
///
/// 父进程不碰门面：它只提供合成环境、拉起子进程，并用**新连接**在阶段之间核对云端事实
/// （写入落库、删除生效、只读零副作用），最后按正常删除路径清理本轮命名空间。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_deployment_entry_point_full_lifecycle() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-facade");
    let home = tempfile::tempdir().expect("temp home");
    let read_only_home = tempfile::tempdir().expect("temp home for the read-only phase");
    let workspace = synthetic_workspace();
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    let result = with_cleanup(&target, &run, || async {
        let lines = lifecycle_flow(
            &target,
            &run,
            home.path(),
            read_only_home.path(),
            workspace.path(),
        )
        .await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("deployment_lifecycle=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn lifecycle_flow(
    target: &CloudTarget,
    run: &str,
    home: &Path,
    read_only_home: &Path,
    workspace: &Path,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    lines.extend(run_child(WRITE_CHILD, home, workspace, run, None)?);
    let written = counts_now(target, run).await?;
    check(
        written.sessions == 3 && written.messages == 6,
        "write phase must save root, child and fork remotely",
    )?;
    check(
        !local_sqlite_path(home).exists(),
        "Turso write opened a local SQLite file",
    )?;
    lines.push(format!(
        "after_write sessions={} messages={}",
        written.sessions, written.messages
    ));

    // A damaged local database must have no bearing on Turso cold recovery.
    let poisoned = local_sqlite_path(home);
    std::fs::create_dir_all(poisoned.parent().expect("SQLite path has a parent"))
        .map_err(|error| error.to_string())?;
    std::fs::write(&poisoned, b"damaged local SQLite").map_err(|error| error.to_string())?;

    lines.extend(run_child(RECOVER_CHILD, home, workspace, run, None)?);
    let recovered = counts_now(target, run).await?;
    check(
        recovered.sessions == 1 && recovered.messages == 3,
        "cold recovery must leave the fork tree remotely",
    )?;
    check(
        std::fs::read(&poisoned).map_err(|error| error.to_string())? == b"damaged local SQLite",
        "Turso cold recovery touched local SQLite",
    )?;
    lines.push(format!(
        "after_recovery sessions={} messages={}",
        recovered.sessions, recovered.messages
    ));

    lines.extend(run_child(
        READ_ONLY_CHILD,
        read_only_home,
        workspace,
        run,
        Some("fresh"),
    )?);
    check(
        !local_sqlite_path(read_only_home).exists(),
        "fresh read-only Turso open created local SQLite",
    )?;
    lines.extend(run_child(
        READ_ONLY_CHILD,
        home,
        workspace,
        run,
        Some("registered"),
    )?);
    check(
        std::fs::read(&poisoned).map_err(|error| error.to_string())? == b"damaged local SQLite",
        "read-only Turso open touched local SQLite",
    )?;
    let after = counts_now(target, run).await?;
    check(
        after.sessions == recovered.sessions
            && after.messages == recovered.messages
            && after.ledger == recovered.ledger,
        "read-only open modified remote facts",
    )?;
    Ok(lines)
}

/// 一次云端计数：每次都用**新连接**读，读完即关（不把父进程的连接留在事实之间）。
async fn counts_now(
    target: &CloudTarget,
    run: &str,
) -> Result<super::cloud_tests::RunCounts, String> {
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    store.close().await.map_err(failure)?;
    Ok(counts)
}

/// 合成 workspace：系统临时目录里的空 git 仓库（与本地生命周期夹具同一形态）。
///
/// 它只提供「工作区发现」需要的稳定证据（根目录与 `.git` 对象身份），不含任何真实项目
/// 文件，也不在仓库内。
pub(super) fn synthetic_workspace() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temp workspace");
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=synthetic",
            "-c",
            "user.email=synthetic@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "synthetic base",
        ],
    );
    directory
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git fixture could not start");
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 拉起一个子进程阶段：进程边界消失之后仍然成立的事实才是 durable 事实。
///
/// 子进程的输出只回传 `PROBE` 行，并再经一次凭证字面量校验；断言失败会原样带上子进程
/// 输出（子进程的失败信息本身已按同一规则脱敏）。
pub(super) fn run_child(
    test: &str,
    home: &Path,
    workspace: &Path,
    run: &str,
    read_only: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new(
        std::env::current_exe()
            .map_err(|error| format!("test binary path is unavailable: {error}"))?,
    );
    command
        .args(["--exact", test, "--nocapture"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(HOME_ENV, home)
        .env(WORKSPACE_ENV, workspace)
        .env(RUN_ENV, run);
    if let Some(mode) = read_only {
        command.env(READ_ONLY_ENV, mode);
    }
    let output = command
        .output()
        .map_err(|error| format!("child process could not start: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let phase = read_only
        .map(|mode| format!("{test} ({mode})"))
        .unwrap_or_else(|| test.to_owned());
    if !output.status.success() || !stdout.contains("test result: ok") {
        return Err(format!(
            "child phase failed: {phase}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ));
    }
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("PROBE "))
        .map(str::to_owned)
        .collect())
}
