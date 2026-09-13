//! FIXSEC（round 4）回归：本产品自产产物路径的 capability 校验（WP-009-r3 的 F2）。
//!
//! 缺口回顾（`artifacts/reviews/WP-009-r3.md` §7 F2）：日志与 `local-tool-output-*.txt`
//! 曾用「字符串拼出的路径 + `std::fs`」直接落盘，因此把 `.local-mcp/logs` 换成指向根外
//! 的符号链接，就能把落盘重定向到授权根之外，而工具仍然报告工作区里的路径。
//!
//! 本文件的断言分两个方向：
//!
//! - **反向（必须拒绝且不写文件）**：先把目录替换成符号链接，再触发落盘/建日志；
//!   目标目录必须保持为空，且返回的落盘路径必须为「失败」（`path = None`）。
//! - **正向（正常路径仍工作）**：不替换目录时，落盘与日志仍在授权根内创建、内容可读、
//!   路径与工具返回文本一致（防止"为了安全把功能关掉"）。
//!
//! 单进程形态（D-003）下日志由唯一任务注册表驱动、直接作用于真实进程，因此这里经
//! 注册表调用（不再有 worker 子进程与帧）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use local_mcp_server::capability::RootDir;
use local_mcp_server::runtime::{InProcessExecutor, LOG_SUBDIR};
use local_mcp_server::tasks::log::{DirOutputPersist, OutputPersist};
use local_mcp_server::tasks::registry::{
    BashRunArgs, TaskAccessError, TaskRegistry, TaskRegistryConfig,
};
use local_mcp_server::tasks::{BashTaskConfig, BashTasks};
use local_mcp_server::wire::RequestContext;

const ARTIFACT_SUBDIR: &str = ".local-mcp/artifacts";

fn temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir")
}

/// 目录下的普通文件（不含子目录与符号链接），用于断言"没有写出文件"。
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        if entry.path().is_file() {
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// 把 `link`（必须不存在）建成指向 `target` 的符号链接。
#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
    assert!(
        link.symlink_metadata()
            .expect("lstat")
            .file_type()
            .is_symlink(),
        "夹具必须把目录换成符号链接：{}",
        link.display()
    );
}

fn context() -> RequestContext {
    RequestContext::new("req-fixsec", "principal-a", "instance-1")
}

/// 生产接线：唯一任务注册表的日志存储必须接在 capability 校验面上。
#[test]
fn production_registry_wires_capability_checked_artifact_paths() {
    let workspace = temp_dir("fixsec-r4-wiring-");
    let root = Arc::new(RootDir::open(workspace.path()).expect("capability root"));
    let logs = local_mcp_server::tasks::log::LogStore::rooted(Arc::clone(&root), LOG_SUBDIR);
    assert!(
        logs.is_capability_checked(),
        "生产接线的日志存储必须经 capability 校验（F2）"
    );
    assert_eq!(
        logs.dir(),
        workspace.path().join(LOG_SUBDIR),
        "日志目录约定不得改变（Bash 返回的路径要能被 Read 打开）"
    );

    // 执行器也必须在同一根上工作（不引入第二套路径判定）。
    let registry = registry_for(workspace.path());
    let executor = InProcessExecutor::new(workspace.path().to_path_buf(), Arc::clone(&registry))
        .expect("执行器");
    assert_eq!(
        std::fs::canonicalize(executor.workspace()).expect("canonicalize 执行器根"),
        std::fs::canonicalize(workspace.path()).expect("canonicalize 工作区"),
        "执行器根必须是启动参数指定的工作区根（macOS 上 tempdir 可能经 /var 符号链接）"
    );

    // 非 capability 构造只用于测试夹具：显式断言两种构造的差异，避免误用。
    let legacy = local_mcp_server::tasks::log::LogStore::new(workspace.path().join("logs"));
    assert!(!legacy.is_capability_checked());
    let rooted = DirOutputPersist::rooted(
        Arc::new(RootDir::open(workspace.path()).expect("root")),
        LOG_SUBDIR,
    );
    assert!(rooted.is_capability_checked());
}

/// 构造接在 capability 面上的注册表（日志目录 = `<workspace>/.local-mcp/logs`）。
fn registry_for(workspace: &Path) -> Arc<TaskRegistry> {
    let root = Arc::new(RootDir::open(workspace).expect("capability root"));
    let artifact = Arc::new(DirOutputPersist::rooted(
        Arc::new(RootDir::open(workspace).expect("capability root")),
        ARTIFACT_SUBDIR,
    ));
    let mut config = TaskRegistryConfig::new(artifact);
    config.poll_interval = None;
    let bash = Arc::new(BashTasks::new(
        BashTaskConfig::new(workspace, workspace.join(LOG_SUBDIR))
            .with_capability(root, LOG_SUBDIR),
    ));
    TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config)
}

/// 反向：落盘目录被替换成符号链接后，落盘必须被 capability 拒绝，
/// 且**不产生任何文件**（既不在链接目标，也不在授权根内）。
#[cfg(unix)]
#[test]
fn truncated_output_persist_refuses_a_symlinked_artifact_dir() {
    let workspace = temp_dir("fixsec-r4-fs-persist-");
    let outside = temp_dir("fixsec-r4-fs-persist-outside-");
    let artifacts = workspace.path().join(ARTIFACT_SUBDIR);
    std::fs::create_dir_all(artifacts.parent().expect("parent")).expect("create .local-mcp");
    symlink_dir(outside.path(), &artifacts);

    let root = Arc::new(RootDir::open(workspace.path()).expect("capability root"));
    let persist = DirOutputPersist::rooted(root, ARTIFACT_SUBDIR);
    let outcome = persist.persist("full output that must never escape the workspace");

    assert!(
        outcome.path.is_none(),
        "符号链接落盘目录必须被拒绝（path=None），实际：{:?}",
        outcome.path
    );
    assert!(
        outcome.hint.contains("Failed to save full output"),
        "失败必须给出明确文本（保持源格式），实际：{}",
        outcome.hint
    );
    assert!(
        outcome.hint.contains("authorized workspace") || outcome.hint.contains("symbolic link"),
        "失败文本必须说明拒绝原因（越界/符号链接），实际：{}",
        outcome.hint
    );
    assert!(
        files_under(outside.path()).is_empty(),
        "符号链接目标目录里不得出现任何文件：{:?}",
        files_under(outside.path())
    );
    assert!(
        artifacts
            .symlink_metadata()
            .expect("lstat")
            .file_type()
            .is_symlink(),
        "夹具符号链接必须原样保留（不得被替换成真实目录或被删除）"
    );

    // 另一条落盘路径（前台超时的部分输出）共享同一实现，也必须拒绝。
    let partial = persist.persist_partial("partial output");
    assert!(partial.path.is_none(), "部分输出同样不得越界落盘");
    assert!(partial.hint.contains("Failed to save partial output"));
    assert!(files_under(outside.path()).is_empty());
}

/// 正向：正常（真实目录）时落盘仍在授权根内完成，路径可读、内容一致。
#[test]
fn truncated_output_persist_still_works_inside_the_root() {
    let workspace = temp_dir("fixsec-r4-fs-persist-ok-");
    let root = Arc::new(RootDir::open(workspace.path()).expect("capability root"));
    let persist = DirOutputPersist::rooted(root, ARTIFACT_SUBDIR);

    let outcome = persist.persist("persisted content\nline-2\n");
    let path = outcome
        .path
        .clone()
        .unwrap_or_else(|| panic!("正常目录必须落盘成功：{}", outcome.hint));
    assert!(
        path.starts_with(workspace.path()),
        "落盘必须在授权根内：{}",
        path.display()
    );
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("local-tool-output-")),
        "文件名必须保持源语义前缀：{}",
        path.display()
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read persisted"),
        "persisted content\nline-2\n"
    );
    assert!(outcome.hint.contains("Full output saved to"));

    // 目录确实在授权根内被创建（capability 的按需建父目录）。
    assert!(workspace.path().join(ARTIFACT_SUBDIR).is_dir());
}

// ───────────────────────── 任务日志（唯一任务注册表） ─────────────────────────

/// 把 tracing 输出写进内存缓冲（诊断断言用；不含任何 secret）。
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 在 tracing 捕获下运行一次真任务启动，并返回（状态, 诊断缓冲）。
fn run_logged<F>(task: F) -> (local_mcp_server::tasks::BashTaskState, String)
where
    F: FnOnce() -> local_mcp_server::tasks::BashTaskState,
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let collector = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer({
            let buffer = Arc::clone(&buffer);
            move || CaptureWriter(Arc::clone(&buffer))
        })
        .finish();
    let state = tracing::subscriber::with_default(collector, task);
    let diagnostics = String::from_utf8_lossy(&buffer.lock().expect("buffer lock")).to_string();
    (state, diagnostics)
}

/// 反向：日志目录被换成符号链接后，不得把日志写到链接目标；
/// 返回的状态必须**诚实**（`stdout_log` 缺失），日志读取必须 fail closed。
#[cfg(unix)]
#[test]
fn task_logs_refuse_a_symlinked_log_dir_and_write_nothing_outside() {
    let workspace = temp_dir("fixsec-r4-log-escape-");
    let outside = temp_dir("fixsec-r4-log-escape-outside-");
    let logs = workspace.path().join(LOG_SUBDIR);
    std::fs::create_dir_all(logs.parent().expect("parent")).expect("create .local-mcp");
    symlink_dir(outside.path(), &logs);

    let registry = registry_for(workspace.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let ctx = context();
    let (state, diagnostics) = run_logged(|| {
        let run = runtime
            .block_on(registry.start_bash(
                &ctx,
                BashRunArgs {
                    command: "echo marker-escape; echo err-escape >&2".to_string(),
                    timeout_ms: None,
                    background: false,
                },
            ))
            .expect("命令本身必须完成（日志不可用只降级）");
        match run {
            local_mcp_server::tasks::BashRun::Finished { state } => *state,
            local_mcp_server::tasks::BashRun::Running { state, .. } => *state,
        }
    });

    assert_eq!(
        state.status,
        local_mcp_server::wire::TaskStatus::Completed,
        "命令必须在日志不可用的情况下照常执行：{state:?}"
    );
    assert!(
        state.stdout_log.is_none(),
        "日志路径被 capability 拒绝时必须诚实报告（不得给出会误导的路径）：{state:?}"
    );
    assert!(
        state.stdout.contains("marker-escape"),
        "输出仍必须通过捕获缓冲返回：{state:?}"
    );

    // 链接目标（授权根之外）必须完全没有文件：既不写日志，也不写截断产物。
    assert!(
        files_under(outside.path()).is_empty(),
        "符号链接目标目录里不得出现任何文件：{:?}",
        files_under(outside.path())
    );
    assert!(
        logs.symlink_metadata()
            .expect("lstat")
            .file_type()
            .is_symlink(),
        "夹具符号链接必须原样保留"
    );
    assert!(
        files_under(workspace.path()).is_empty(),
        "授权根内也不得出现日志文件（被拒后就该什么都不写）：{:?}",
        files_under(workspace.path())
    );

    // 诊断：拒绝原因必须出现在本产品的诊断输出里（便于宿主侧溯源），
    // 且不得泄露授权根之外的宿主真实路径。
    let diag = diagnostics
        .lines()
        .find(|line| line.contains("capability"))
        .unwrap_or_else(|| panic!("诊断必须记录 capability 拒绝，实际诊断：{diagnostics}"));
    assert!(
        !diag.contains(&outside.path().to_string_lossy().to_string()),
        "诊断不得回显授权根之外的宿主路径：{diag}"
    );
}

/// 正向：日志目录正常时，日志写在授权根内、内容可读、路径与报告一致。
#[cfg(unix)]
#[test]
fn task_logs_are_written_inside_the_root_and_readable() {
    let workspace = temp_dir("fixsec-r4-log-ok-");
    let registry = registry_for(workspace.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let ctx = context();
    let run = runtime
        .block_on(registry.start_bash(
            &ctx,
            BashRunArgs {
                command: "echo marker-ok; echo err-ok >&2".to_string(),
                timeout_ms: None,
                background: false,
            },
        ))
        .expect("Bash 必须成功");
    let state = match run {
        local_mcp_server::tasks::BashRun::Finished { state } => *state,
        local_mcp_server::tasks::BashRun::Running { state, .. } => *state,
    };

    let stdout_log = state
        .stdout_log
        .as_deref()
        .unwrap_or_else(|| panic!("正常路径必须报告日志路径：{state:?}"))
        .to_string();
    let path = PathBuf::from(&stdout_log);
    assert!(
        path.starts_with(workspace.path()),
        "日志必须落在授权根内：{}",
        path.display()
    );
    assert!(path.is_file(), "日志文件必须真实存在：{}", path.display());
    assert!(
        std::fs::read_to_string(&path)
            .expect("read log")
            .contains("marker-ok"),
        "日志内容必须包含命令输出"
    );
}

/// 正向：注册表能读到同一份日志（句柄由注册表在进程内注入）。
#[cfg(unix)]
#[test]
fn registered_task_logs_are_readable_through_the_registry() {
    let workspace = temp_dir("fixsec-r4-log-read-ok-");
    let registry = registry_for(workspace.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let ctx = context();
    // 后台任务进入注册表（前台完成的任务不占用表），日志写入后可经注册表读取。
    //
    // 整段逻辑必须在**同一个** `block_on` 内：后台任务的排空/监督任务由 tokio 调度，
    // 离开运行时后它们不会被推进（日志也就不会落盘）。
    runtime.block_on(async {
        let run = registry
            .start_bash(
                &ctx,
                BashRunArgs {
                    command: "echo marker-registry; sleep 1".to_string(),
                    timeout_ms: None,
                    background: true,
                },
            )
            .await
            .expect("Bash 必须成功");
        let task_id = match run {
            local_mcp_server::tasks::BashRun::Running { snapshot, .. } => snapshot.task_id.clone(),
            local_mcp_server::tasks::BashRun::Finished { state } => state.task_id.clone(),
        };

        let mut payload = None;
        for _ in 0..100 {
            match registry
                .read_log(
                    &ctx,
                    &task_id,
                    local_mcp_server::tasks::LogStream::Stdout,
                    4_096,
                )
                .await
            {
                Ok(read) if read.content.contains("marker-registry") => {
                    payload = Some(read);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        let payload = payload.expect("正向路径的日志读取必须成功");
        assert!(payload.content.contains("marker-registry"), "{payload:?}");

        // 跨身份读取仍被拒绝（授权层不因"句柄在进程内"而放宽）。
        let other = RequestContext::new("req-x", "principal-b", "instance-1");
        let error = registry
            .read_log(
                &other,
                &task_id,
                local_mcp_server::tasks::LogStream::Stdout,
                4_096,
            )
            .await
            .expect_err("跨主体读日志必须拒绝");
        assert!(matches!(error, TaskAccessError::ForeignOwner { .. }));
    });
}
