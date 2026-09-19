//! Bounded Git discovery and conservative local filesystem identity evidence.

use anyhow::{Context, Result};
use peri_acp_types::workspace::WorkspaceError;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ObjectIdentity {
    device: u64,
    inode: u64,
}

/// Decode an identity written by schema 3 and emit the schema 4 canonical
/// form. Creation timestamps are deliberately accepted only as legacy input;
/// they are not part of identity or compared during discovery.
pub(super) fn normalize_identity_json(value: &serde_json::Value) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity".into()))?;
    if !object.keys().all(|key| {
        matches!(
            key.as_str(),
            "device" | "inode" | "birth_seconds" | "birth_nanos"
        )
    }) {
        return Err(WorkspaceError::DiscoveryError("unknown object identity field".into()).into());
    }
    let device = object
        .get("device")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity device".into()))?;
    let inode = object
        .get("inode")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity inode".into()))?;
    serde_json::to_string(&ObjectIdentity { device, inode }).map_err(Into::into)
}

pub(super) fn normalize_discovery_json(value: &serde_json::Value) -> Result<String> {
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid discovery snapshot".into()))?;
    if !object.keys().all(|key| {
        matches!(
            key.as_str(),
            "root"
                | "root_identity"
                | "common_dir"
                | "common_identity"
                | "private_dir"
                | "private_identity"
        )
    }) {
        return Err(
            WorkspaceError::DiscoveryError("unknown discovery snapshot field".into()).into(),
        );
    }
    for key in ["root_identity", "common_identity", "private_identity"] {
        if let Some(identity) = object.get(key) {
            if identity.is_null() {
                continue;
            }
            let normalized = normalize_identity_json(identity)?;
            object.insert(key.into(), serde_json::from_str(&normalized)?);
        }
    }
    let discovery: Discovery = serde_json::from_value(value)?;
    serde_json::to_string(&discovery).map_err(Into::into)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Discovery {
    pub root: PathBuf,
    pub root_identity: ObjectIdentity,
    pub common_dir: Option<PathBuf>,
    pub common_identity: Option<ObjectIdentity>,
    pub private_dir: Option<PathBuf>,
    pub private_identity: Option<ObjectIdentity>,
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        WorkspaceError::DiscoveryError("non-UTF-8 paths are unsupported".into()).into()
    })
}

pub(super) async fn object_identity(path: &Path) -> Result<ObjectIdentity> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| WorkspaceError::Unavailable)?;
    if !meta.is_dir() {
        return Err(WorkspaceError::Unavailable.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ObjectIdentity {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || windows_object_identity(&path)).await?
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        Err(WorkspaceError::Unsupported.into())
    }
}

#[cfg(windows)]
fn windows_object_identity(path: &Path) -> Result<ObjectIdentity> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The owned directory handle remains valid throughout this call, and the API
    // initializes the output only on success. No handle is transferred or inherited.
    let success =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let information = unsafe { information.assume_init() };
    Ok(ObjectIdentity {
        device: u64::from(information.dwVolumeSerialNumber),
        inode: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

fn git_command_path(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let path = path_text(path)?;
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        Ok(PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path)))
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

async fn bounded_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let mut data = Vec::new();
    reader.take(MAX_BYTES + 1).read_to_end(&mut data).await?;
    if data.len() as u64 > MAX_BYTES {
        return Err(WorkspaceError::DiscoveryError(
            "Git discovery output exceeded its bound".into(),
        )
        .into());
    }
    Ok(data)
}

/// 一次 Git 子进程启动的尝试次数。
///
/// `Command::spawn` 失败意味着没有子进程被启动，重试不改变语义；而负载下的启动
/// 失败多是瞬时的资源不足（EAGAIN/ENOMEM 等），尤其在 CI 这类并行度高的环境。
/// 一次瞬时抖动就让发现失败，用户当次会话就建不起来——代价远大于退让重试。
const GIT_SPAWN_ATTEMPTS: u32 = 3;

/// 启动 Git 子进程；瞬时失败退让后重试。
///
/// `NotFound` 是确定性的环境事实（Git 未安装），由调用方按「Git 不可用」处理，
/// 不重试；其余错误重试到次数上限后仍按原语义上报「Git 不可执行」。
async fn spawn_git(command: &mut Command) -> Result<Option<tokio::process::Child>> {
    let mut attempt = 1;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(Some(child)),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) if attempt < GIT_SPAWN_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
            }
            Err(_) => {
                return Err(
                    WorkspaceError::DiscoveryError("Git could not be executed".into()).into(),
                );
            }
        }
    }
}

async fn git(program: &OsStr, cwd: &Path, args: &[&str]) -> Result<Option<std::process::Output>> {
    let mut command = Command::new(program);
    command
        .arg("-C")
        .arg(git_command_path(cwd)?)
        .args(args)
        .kill_on_drop(true)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(name);
        }
    }
    let Some(mut child) = spawn_git(&mut command).await? else {
        return Ok(None);
    };
    let stdout = child.stdout.take().context("Git stdout unavailable")?;
    let stderr = child.stderr.take().context("Git stderr unavailable")?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (stdout, stderr, status) =
            tokio::try_join!(bounded_output(stdout), bounded_output(stderr), async {
                child.wait().await.map_err(anyhow::Error::from)
            })?;
        Ok::<_, anyhow::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(Some(output)),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(WorkspaceError::DiscoveryError("Git discovery timed out".into()).into())
        }
    }
}

/// 一次 `rev-parse` 解析多个位置：输出按参数顺序每行一个。
///
/// 两个路径来自同一次调用而不是两次进程启动：准入路径会放大每次发现的外部进程
/// 数量，位置之间也没有需要分开处理的语义。
///
/// 只请求 `--show-toplevel` / `--git-dir` 这类旧版 Git 也认得的选项。这里不用
/// `--path-format=absolute` 与 `--absolute-git-dir`（上游文档记为 Git 2.31 / 2.13
/// 引入），也不请求 `--git-common-dir`（Git 2.5 引入，见 `common_directory`）。代价是
/// 位置的默认输出可能是相对路径——且同一命令在不同 cwd 下的输出形式不同（主仓库根
/// 给相对 `.git`，子目录的 `--git-dir` 反而给绝对路径）——因此两类输出逐个判断后
/// 都要与 cwd 组合再 canonicalize，不能直接按宿主进程的 cwd 解释。
async fn git_paths(
    program: &OsStr,
    cwd: &Path,
    args: &[&str],
    count: usize,
) -> Result<Vec<PathBuf>> {
    let output = git(program, cwd, args).await?.ok_or_else(|| {
        WorkspaceError::DiscoveryError("Git became unavailable during discovery".into())
    })?;
    if !output.status.success() {
        return Err(WorkspaceError::DiscoveryError("Git location discovery failed".into()).into());
    }
    let text = String::from_utf8(output.stdout).context("Git path is not UTF-8")?;
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    if lines.len() != count {
        // 行数与请求不符说明输出顺序不再可信，不能把错位的位置当成根目录。
        return Err(WorkspaceError::DiscoveryError(
            "Git location discovery returned an unexpected number of paths".into(),
        )
        .into());
    }
    let mut paths = Vec::with_capacity(count);
    for line in lines {
        // 旧版 Git 不认识选项时会把选项原文回显到 stdout（rev-parse 把未知参数当普通
        // 参数输出），若不拦住就会当成相对路径拼在 cwd 下，最终以「目录不可用」掩盖
        // 真正的版本问题。
        if line.starts_with("--") {
            return Err(WorkspaceError::DiscoveryError(
                "Git location discovery returned an unknown option".into(),
            )
            .into());
        }
        let path = Path::new(line);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        paths.push(
            tokio::fs::canonicalize(path)
                .await
                .map_err(|_| WorkspaceError::Unavailable)?,
        );
    }
    Ok(paths)
}

/// Git 回答「这个路径不在仓库里」：只有这种失败是「不是仓库」的证据。
///
/// 文案随版本变化。本机真实 Git 2.4.12 回答
/// `fatal: Not a git repository (or any of the parent directories): .git`，在它读不了的
/// linked worktree 里回答 `fatal: Not a git repository: <gitdir>`（都是大写 `N`）；
/// 2.39 起改为小写 `not`。按大小写敏感匹配会把旧版 Git 下的**普通目录**判成
/// `Git rejected repository discovery`，于是那个版本的用户连非仓库目录都建不了会话——
/// 而 Git 已经明确回答「不是仓库」，本应得到目录项目（设计 §3.3）。
///
/// 放宽的只是大小写，不是匹配范围：真实拒绝（权限、unsafe repository、损坏仓库）的
/// 文案不含这个前缀，仍然原样上报。
fn is_not_a_repository(stderr: &[u8]) -> bool {
    const PREFIX: &[u8] = b"fatal: not a git repository";
    stderr.len() >= PREFIX.len() && stderr[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
}

/// 旧版 Git 不认识新选项时走用法错误（打印 usage 并非零退出），而不是给出探测结果。
///
/// 只有这种失败才允许退回兼容参数；真实失败（权限、损坏仓库等）必须原样上报，
/// 不能因为退回而看起来已经发现成功。
fn is_usage_error(output: &std::process::Output) -> bool {
    !output.status.success()
        && output
            .stderr
            .windows(b"usage:".len())
            .any(|window| window == b"usage:")
}

/// 旧版 Git 整个子命令都不存在时的回答（`git: 'worktree' is not a git command.`）。
fn is_missing_command(output: &std::process::Output) -> bool {
    !output.status.success()
        && output
            .stderr
            .windows(b"is not a git command".len())
            .any(|window| window == b"is not a git command")
}

/// 一次 `worktree list --porcelain` 的结果。
enum WorktreeMembership {
    /// Git 给出了成员列表与字段分隔符（`-z` 不可用时为换行）。
    Listed(std::process::Output, u8),
    /// Git 没有这个子命令或选项：没有成员列表可核对。
    Unsupported,
}

/// 一次 `worktree list --porcelain`，附带字段分隔符。
///
/// 优先 `-z`：NUL 分隔能承载含换行的路径。旧版 Git 没有 `-z`，会按用法错误退出，
/// 此时退回换行分隔；退回只改变分隔符，成员判定仍由调用方按完整路径精确比对。
/// 子命令或选项整体不存在时返回 `Unsupported`：位置已经由 `rev-parse` 回答，跳过的
/// 只是交叉核对，真实失败（损坏仓库、权限）仍必须原样上报。
async fn git_worktree_membership(program: &OsStr, cwd: &Path) -> Result<WorktreeMembership> {
    let unavailable = || {
        anyhow::Error::from(WorkspaceError::DiscoveryError(
            "Git became unavailable during discovery".into(),
        ))
    };
    let primary = git(program, cwd, &["worktree", "list", "--porcelain", "-z"])
        .await?
        .ok_or_else(unavailable)?;
    if is_missing_command(&primary) {
        return Ok(WorktreeMembership::Unsupported);
    }
    if !is_usage_error(&primary) {
        return Ok(WorktreeMembership::Listed(primary, 0));
    }
    let fallback = git(program, cwd, &["worktree", "list", "--porcelain"])
        .await?
        .ok_or_else(unavailable)?;
    if is_usage_error(&fallback) {
        // 连 `--porcelain` 都不存在（`worktree list` 刚出现的版本）：没有成员列表可核对。
        return Ok(WorktreeMembership::Unsupported);
    }
    Ok(WorktreeMembership::Listed(fallback, b'\n'))
}

/// common Git directory：读 Git 自己写入的 `commondir` 文件。
///
/// `gitrepository-layout` 把这个文件的语义定义为 `$GIT_COMMON_DIR`（相对路径按
/// `$GIT_DIR` 解析），`rev-parse --git-common-dir` 是它的投影。这里不请求那个选项，
/// 因为它从 Git 2.5 起才存在，旧版把未知选项原样回显，位置会被当成不存在的路径。
/// linked worktree 由 `git worktree add` 写入该文件；主工作树没有它，两个位置相同。
async fn common_directory(private_dir: &Path) -> Result<PathBuf> {
    let marker = private_dir.join("commondir");
    let text = match tokio::fs::read_to_string(&marker).await {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(private_dir.to_path_buf()),
        Err(_) => {
            return Err(
                WorkspaceError::DiscoveryError("Git common directory is unreadable".into()).into(),
            )
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(
            WorkspaceError::DiscoveryError("Git common directory is unreadable".into()).into(),
        );
    }
    let path = Path::new(text);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        private_dir.join(path)
    };
    tokio::fs::canonicalize(&path).await.map_err(|_| {
        anyhow::Error::from(WorkspaceError::DiscoveryError(
            "Git common directory is unavailable".into(),
        ))
    })
}

/// 一次目录观测：`Discovery` 加上「Git 是否真的回答过」这一证据强度标记。
///
/// Git 不可用时得到的是不完整的目录模式观测：它不能证明该路径不是仓库，因此
/// 登记层不得据它改写已登记的 Git 布局。
#[derive(Debug)]
pub(super) struct Observation {
    pub(super) discovery: Discovery,
    pub(super) git_answered: bool,
}

pub(super) async fn observe(cwd: &Path) -> Result<(PathBuf, Observation)> {
    observe_with_git(cwd, OsStr::new("git")).await
}

async fn observe_with_git(cwd: &Path, program: &OsStr) -> Result<(PathBuf, Observation)> {
    let cwd = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| WorkspaceError::Unavailable)?;
    path_text(&cwd)?;
    let cwd_identity = object_identity(&cwd).await?;
    let inside = git(program, &cwd, &["rev-parse", "--is-inside-work-tree"]).await?;
    // `Some` 表示 Git 给出了回答（即使回答是「不是仓库」）；`None` 表示 Git 不可用。
    let git_answered = inside.is_some();
    if let Some(output) = &inside {
        if !output.status.success() && !is_not_a_repository(&output.stderr) {
            return Err(
                WorkspaceError::DiscoveryError("Git rejected repository discovery".into()).into(),
            );
        }
    }
    // Missing Git is a directory-only execution mode, not evidence that the path
    // is outside a repository. Persisted Git bindings still require the exact
    // discovery snapshot in revalidate, and the registry never rewrites one from
    // an observation without `git_answered`; never rewrite them to this directory.
    let Some(inside) = inside.filter(|output| output.status.success()) else {
        return Ok((
            cwd.clone(),
            Observation {
                discovery: Discovery {
                    root_identity: cwd_identity,
                    root: cwd,
                    common_dir: None,
                    common_identity: None,
                    private_dir: None,
                    private_identity: None,
                },
                git_answered,
            },
        ));
    };
    if inside.stdout != b"true\n" {
        return Err(WorkspaceError::DiscoveryError(
            "bare repositories are not execution workspaces".into(),
        )
        .into());
    }
    // 两个位置共用一次 `rev-parse`：输出顺序与参数顺序一致。common directory 由
    // `commondir` 文件解析，见 `common_directory`。
    let [root, private_dir] = git_paths(
        program,
        &cwd,
        &["rev-parse", "--show-toplevel", "--git-dir"],
        2,
    )
    .await?
    .try_into()
    .map_err(|_| {
        anyhow::Error::from(WorkspaceError::DiscoveryError(
            "Git location discovery returned an unexpected number of paths".into(),
        ))
    })?;
    let common_dir = common_directory(&private_dir).await?;
    if let WorktreeMembership::Listed(worktrees, separator) =
        git_worktree_membership(program, &cwd).await?
    {
        let expected = git_command_path(&root)?;
        if !worktrees.status.success()
            || !worktrees
                .stdout
                .split(|byte| *byte == separator)
                .any(|field| {
                    field
                        .strip_prefix(b"worktree ")
                        .and_then(|path| std::str::from_utf8(path).ok())
                        .is_some_and(|path| Path::new(path) == expected)
                })
        {
            return Err(WorkspaceError::DiscoveryError(
                "Git worktree membership is inconsistent".into(),
            )
            .into());
        }
    }
    let discovered = Discovery {
        root_identity: object_identity(&root).await?,
        common_identity: Some(object_identity(&common_dir).await?),
        private_identity: Some(object_identity(&private_dir).await?),
        root,
        common_dir: Some(common_dir),
        private_dir: Some(private_dir),
    };
    Ok((
        cwd,
        Observation {
            discovery: discovered,
            git_answered,
        },
    ))
}

impl Discovery {
    pub async fn revalidate(&self, cwd: &Path) -> Result<()> {
        self.revalidate_with_git(cwd, OsStr::new("git")).await
    }
    async fn revalidate_with_git(&self, cwd: &Path, program: &OsStr) -> Result<()> {
        let (_, current) = observe_with_git(cwd, program).await?;
        if current.discovery != *self {
            return Err(WorkspaceError::NeedsRelink.into());
        }
        Ok(())
    }

    /// 提交前的重验：只复核外部探测所依赖的关键文件对象，不启动任何外部进程。
    ///
    /// 设计 §3.2 要求探测在事务外进行、提交前只复核关键文件对象与关联关系。Git
    /// 布局是同一目录的派生观测，登记层会在下一次准入刷新它，因此在持有 SQLite
    /// 写事务期间重新执行完整发现既无必要，也会把 Git 的等待时间摊到同库其他
    /// writer 身上。
    pub(super) async fn reassert_key_objects(&self, cwd: &Path) -> Result<()> {
        let canonical = tokio::fs::canonicalize(cwd)
            .await
            .map_err(|_| WorkspaceError::Unavailable)?;
        if canonical != cwd {
            return Err(WorkspaceError::NeedsRelink.into());
        }
        if object_identity(&self.root).await? != self.root_identity {
            return Err(WorkspaceError::NeedsRelink.into());
        }
        for (path, recorded) in [
            (self.common_dir.as_deref(), self.common_identity.as_ref()),
            (self.private_dir.as_deref(), self.private_identity.as_ref()),
        ] {
            match (path, recorded) {
                // 记录过的 Git 位置被移除或替换：证据不足，放弃本次结果。
                (Some(path), Some(recorded)) => match object_identity(path).await {
                    Ok(current) if current == *recorded => {}
                    _ => return Err(WorkspaceError::NeedsRelink.into()),
                },
                (None, None) => {}
                // 快照自相矛盾：路径与文件对象身份必须成对出现。
                _ => return Err(WorkspaceError::InvalidBinding.into()),
            }
        }
        Ok(())
    }

    pub fn project_locator(&self) -> &Path {
        self.common_dir.as_deref().unwrap_or(&self.root)
    }
    pub fn project_identity(&self) -> &ObjectIdentity {
        self.common_identity.as_ref().unwrap_or(&self.root_identity)
    }
}

#[cfg(test)]
#[path = "discovery_test.rs"]
mod tests;
