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
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(WorkspaceError::DiscoveryError("Git could not be executed".into()).into());
        }
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
/// 三个路径来自同一次调用而不是三次进程启动：准入路径会放大每次发现的外部进程
/// 数量，位置之间也没有需要分开处理的语义。
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
        paths.push(
            tokio::fs::canonicalize(line)
                .await
                .map_err(|_| WorkspaceError::Unavailable)?,
        );
    }
    Ok(paths)
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
        if !output.status.success() && !output.stderr.starts_with(b"fatal: not a git repository (")
        {
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
    // 三个位置共用一次 `rev-parse`：输出顺序与参数顺序一致。
    let [root, common_dir, private_dir] = git_paths(
        program,
        &cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--absolute-git-dir",
        ],
        3,
    )
    .await?
    .try_into()
    .map_err(|_| {
        anyhow::Error::from(WorkspaceError::DiscoveryError(
            "Git location discovery returned an unexpected number of paths".into(),
        ))
    })?;
    let worktrees = git(program, &cwd, &["worktree", "list", "--porcelain", "-z"])
        .await?
        .ok_or_else(|| {
            WorkspaceError::DiscoveryError("Git became unavailable during discovery".into())
        })?;
    let expected = git_command_path(&root)?;
    if !worktrees.status.success()
        || !worktrees.stdout.split(|byte| *byte == 0).any(|field| {
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
