//! Stdio subprocesses remain owned by the session pool through handshake cancellation and close.
use peri_process::ProcessTree;
use rmcp::{
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{async_rw::AsyncRwTransport, Transport},
    RoleClient,
};
use std::{
    collections::HashMap,
    future::Future,
    io,
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
    task::JoinHandle,
};

pub(crate) struct McpProcessOwner {
    tree: Arc<ProcessTree>,
    child: Mutex<Child>,
    stderr: Mutex<Option<JoinHandle<()>>>,
    close: Mutex<()>,
    stopped: AtomicBool,
}

impl McpProcessOwner {
    pub(crate) fn begin_close(&self) {
        self.tree.terminate();
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) async fn close(&self) -> io::Result<()> {
        let _closing = self.close.lock().await;
        if self.is_stopped() {
            return Ok(());
        }
        self.begin_close();
        // Tokio wait is cancellation safe; the original handle remains in this owner.
        self.child.lock().await.wait().await?;
        self.tree.wait_for_exit().await;
        let mut stderr = self.stderr.lock().await;
        if let Some(task) = stderr.as_mut() {
            if let Err(error) = task.await {
                tracing::warn!(%error, "MCP stderr reader stopped unexpectedly");
            }
            stderr.take();
        }
        self.stopped.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for McpProcessOwner {
    fn drop(&mut self) {
        self.tree.terminate();
        if let Some(task) = self.stderr.get_mut().take() {
            task.abort();
        }
    }
}

pub(crate) struct McpStdioTransport {
    io: AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>,
    owner: Arc<McpProcessOwner>,
}

impl McpStdioTransport {
    pub(crate) fn process_owner(&self) -> Arc<McpProcessOwner> {
        self.owner.clone()
    }
}

impl Drop for McpStdioTransport {
    fn drop(&mut self) {
        self.owner.begin_close();
    }
}

impl Transport<RoleClient> for McpStdioTransport {
    type Error = io::Error;
    fn send(
        &mut self,
        message: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.io.send(message)
    }
    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.io.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        self.owner.begin_close();
        self.io.close().await?;
        self.owner.close().await
    }
}

impl super::McpClientPool {
    pub(crate) fn spawn_stdio_transport(
        &self,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: &Path,
    ) -> io::Result<McpStdioTransport> {
        let arg_strs: Vec<_> = args.iter().map(String::as_str).collect();
        let mut cmd = peri_agent::agent::async_tasks::shell_command(command, &arg_strs);
        cmd.envs(env).current_dir(cwd);
        self.spawn_process_command(cmd, Some(command))
    }

    pub(crate) fn spawn_process_command(
        &self,
        mut cmd: tokio::process::Command,
        stderr_label: Option<&str>,
    ) -> io::Result<McpStdioTransport> {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return Err(io::Error::other("MCP pool is closing"));
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if stderr_label.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut tree = ProcessTree::new()?;
        tree.prepare(&mut cmd);
        let mut child = cmd.spawn()?;
        let attached = tree.attach(&child);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take().map(|stderr| {
            let command = stderr_label.unwrap_or("MCP").to_owned();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(%command, %line, "MCP subprocess stderr");
                }
            })
        });
        let owner = Arc::new(McpProcessOwner {
            tree: Arc::new(tree),
            child: Mutex::new(child),
            stderr: Mutex::new(stderr),
            close: Mutex::new(()),
            stopped: AtomicBool::new(false),
        });
        let mut processes = self.processes.lock();
        processes.retain(|process| !process.is_stopped());
        processes.push(owner.clone());
        drop(processes);
        if let Err(error) = attached {
            owner.begin_close();
            return Err(error);
        }
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            owner.begin_close();
            return Err(io::Error::other("MCP child stdio is unavailable"));
        };
        Ok(McpStdioTransport {
            io: AsyncRwTransport::new(stdout, stdin),
            owner,
        })
    }

    pub(super) async fn close_processes(&self) -> usize {
        let processes = self.processes.lock().clone();
        let mut unfinished = 0;
        for process in processes {
            if !matches!(
                tokio::time::timeout(super::SHUTDOWN_TIMEOUT, process.close()).await,
                Ok(Ok(()))
            ) {
                unfinished += 1;
            }
        }
        self.processes
            .lock()
            .retain(|process| !process.is_stopped());
        unfinished
    }
}

#[cfg(all(test, unix))]
#[path = "process_test.rs"]
mod tests;
