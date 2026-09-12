//! Bounded Git discovery and conservative local filesystem identity evidence.

use anyhow::{Context, Result};
use peri_acp_types::workspace::WorkspaceError;
use serde::{Deserialize, Serialize};
use std::{
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
    birth_seconds: u64,
    birth_nanos: u32,
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
        // Birth time strengthens inode evidence without depending on mutable directory mtime.
        // Filesystems that cannot provide it cannot safely establish this registry contract.
        let birth = meta
            .created()
            .map_err(|_| {
                WorkspaceError::DiscoveryError(
                    "filesystem does not provide creation identity".into(),
                )
            })?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| WorkspaceError::NeedsRelink)?;
        Ok(ObjectIdentity {
            device: meta.dev(),
            inode: meta.ino(),
            birth_seconds: birth.as_secs(),
            birth_nanos: birth.subsec_nanos(),
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
    let birth = (u64::from(information.ftCreationTime.dwHighDateTime) << 32)
        | u64::from(information.ftCreationTime.dwLowDateTime);
    if birth == 0 {
        return Err(WorkspaceError::NeedsRelink.into());
    }
    Ok(ObjectIdentity {
        device: u64::from(information.dwVolumeSerialNumber),
        inode: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        birth_seconds: birth / 10_000_000,
        birth_nanos: ((birth % 10_000_000) * 100) as u32,
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

async fn git(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new("git");
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
    let mut child = command
        .spawn()
        .map_err(|_| WorkspaceError::DiscoveryError("Git could not be executed".into()))?;
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
        Ok(Ok(output)) => Ok(output),
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

async fn git_path(cwd: &Path, args: &[&str]) -> Result<PathBuf> {
    let output = git(cwd, args).await?;
    if !output.status.success() {
        return Err(WorkspaceError::DiscoveryError("Git location discovery failed".into()).into());
    }
    let text = String::from_utf8(output.stdout).context("Git path is not UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    tokio::fs::canonicalize(text)
        .await
        .map_err(|_| WorkspaceError::Unavailable.into())
}

pub(super) async fn discover(cwd: &Path) -> Result<(PathBuf, Discovery)> {
    let cwd = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| WorkspaceError::Unavailable)?;
    path_text(&cwd)?;
    object_identity(&cwd).await?;
    let inside = git(&cwd, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !inside.status.success() {
        if !inside.stderr.starts_with(b"fatal: not a git repository (") {
            return Err(
                WorkspaceError::DiscoveryError("Git rejected repository discovery".into()).into(),
            );
        }
        return Ok((
            cwd.clone(),
            Discovery {
                root_identity: object_identity(&cwd).await?,
                root: cwd,
                common_dir: None,
                common_identity: None,
                private_dir: None,
                private_identity: None,
            },
        ));
    }
    if inside.stdout != b"true\n" {
        return Err(WorkspaceError::DiscoveryError(
            "bare repositories are not execution workspaces".into(),
        )
        .into());
    }
    let root = git_path(
        &cwd,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    )
    .await?;
    let common_dir = git_path(
        &cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let private_dir = git_path(&cwd, &["rev-parse", "--absolute-git-dir"]).await?;
    let worktrees = git(&cwd, &["worktree", "list", "--porcelain", "-z"]).await?;
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
    Ok((cwd, discovered))
}

impl Discovery {
    pub async fn revalidate(&self, cwd: &Path) -> Result<()> {
        let (_, current) = discover(cwd).await?;
        if &current != self {
            return Err(WorkspaceError::NeedsRelink.into());
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
