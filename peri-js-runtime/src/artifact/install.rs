//! npm 安装子进程 owner：取消/安装超时均先回收进程树，再 join stderr。

use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{npm_command, Installer, INSTALL_TIMEOUT, MAX_INSTALL_STDERR_BYTES};
use crate::process_tree::ProcessTree;

pub(super) struct NpmInstaller;

#[async_trait::async_trait]
impl Installer for NpmInstaller {
    async fn install(&self, staging: &Path, cancel: &CancellationToken) -> io::Result<bool> {
        let home = staging.join(".npm-home");
        let cache = staging.join(".npm-cache");
        tokio::fs::create_dir(&home).await?;
        tokio::fs::create_dir(&cache).await?;
        run_install(npm_command(staging, &home, &cache), cancel).await
    }
}

struct InstallProcess {
    child: Child,
    tree: ProcessTree,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
}

impl InstallProcess {
    async fn spawn(mut command: Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        #[cfg(unix)]
        let tree = ProcessTree::new(
            child
                .id()
                .ok_or_else(|| io::Error::other("missing npm pid"))?,
        );
        #[cfg(windows)]
        let tree = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("missing npm process handle"))
            .and_then(|handle| ProcessTree::new(handle as _));
        let tree = match tree {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };
        let mut stderr = child.stderr.take().expect("npm stderr configured as piped");
        let stderr = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stderr.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..count]);
                if tail.len() > MAX_INSTALL_STDERR_BYTES {
                    tail.drain(..tail.len() - MAX_INSTALL_STDERR_BYTES);
                }
            }
            Ok(tail)
        });
        Ok(Self {
            child,
            tree,
            stderr,
        })
    }

    async fn finish(&mut self) -> io::Result<Vec<u8>> {
        let signal = self.tree.terminate(Duration::from_millis(100)).await;
        if signal.is_err() {
            let _ = self.child.start_kill();
        }
        let reaped = self.child.wait().await;
        // macOS can report EPERM for an already-zombie group leader; retry after reap.
        let signal = match signal {
            Ok(()) => Ok(()),
            Err(_) => self.tree.terminate(Duration::from_millis(100)).await,
        };
        let stderr = (&mut self.stderr)
            .await
            .map_err(|_| io::Error::other("npm stderr task failed"))?;
        reaped?;
        signal?;
        stderr
    }
}

impl Drop for InstallProcess {
    fn drop(&mut self) {
        self.stderr.abort();
        let _ = self.child.start_kill();
        // ProcessTree::drop is the cancellation fail-safe if the owner future is dropped.
    }
}

async fn run_install(command: Command, cancel: &CancellationToken) -> io::Result<bool> {
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let mut process = InstallProcess::spawn(command).await?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(io::Error::from(io::ErrorKind::Interrupted)),
        _ = tokio::time::sleep(INSTALL_TIMEOUT) => Ok(false),
        result = process.child.wait() => result.map(|status| status.success()),
    };
    let stderr = process.finish().await?;
    if matches!(result, Ok(false)) {
        debug!(
            stderr_tail_bytes = stderr.len(),
            "PTC npm install failed or timed out"
        );
    }
    result
}

#[cfg(all(test, unix))]
#[path = "install_test.rs"]
mod tests;
