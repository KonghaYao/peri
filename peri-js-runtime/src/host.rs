use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::process_tree::ProcessTree;
use crate::rpc::spawn_stdout_reader;
use crate::{IncomingMessage, JsRuntimeError, Result, RpcChannel};

const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const STDERR_CHUNK_BYTES: usize = 8 * 1024;
const STDERR_TAIL_BYTES: usize = 32 * 1024;

#[derive(Clone)]
pub struct JsProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    inherit_environment: bool,
    environment: Vec<(String, String)>,
}

impl JsProcessSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: None,
            inherit_environment: true,
            environment: Vec::new(),
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub(crate) fn without_inherited_environment(mut self) -> Self {
        self.inherit_environment = false;
        self
    }

    pub(crate) fn with_environment(
        mut self,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }
}

impl std::fmt::Debug for JsProcessSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsProcessSpec")
            .field("program", &self.program)
            .field("args", &"[REDACTED]")
            .field("cwd", &self.cwd.as_ref().map(|_| "[REDACTED]"))
            .field("inherit_environment", &self.inherit_environment)
            .finish()
    }
}

pub struct JsExecutionHost {
    child: tokio::sync::Mutex<Child>,
    channel: Arc<RpcChannel>,
    incoming: tokio::sync::Mutex<Option<mpsc::Receiver<IncomingMessage>>>,
    process_tree: ProcessTree,
    stdout_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    stderr_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    stderr_bytes: Arc<AtomicUsize>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
}

impl JsExecutionHost {
    pub fn spawn(spec: JsProcessSpec) -> Result<Self> {
        Self::spawn_with_frame_limit(spec, DEFAULT_MAX_FRAME_BYTES)
    }

    pub(crate) fn spawn_with_frame_limit(
        spec: JsProcessSpec,
        max_frame_bytes: usize,
    ) -> Result<Self> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        if !spec.inherit_environment {
            command.env_clear();
        }
        command.envs(spec.environment);
        if let Some(cwd) = spec.cwd {
            command.current_dir(cwd);
        }

        let mut child = command
            .spawn()
            .map_err(|error| JsRuntimeError::SpawnFailed(error.to_string()))?;
        #[cfg(unix)]
        let child_id = child
            .id()
            .ok_or_else(|| JsRuntimeError::SpawnFailed("child pid unavailable".into()))?;
        #[cfg(unix)]
        let process_tree = ProcessTree::new(child_id)
            .map_err(|error| JsRuntimeError::SpawnFailed(error.to_string()))?;
        #[cfg(windows)]
        let process_tree = ProcessTree::new(
            child
                .raw_handle()
                .ok_or_else(|| JsRuntimeError::SpawnFailed("child handle unavailable".into()))?
                as _,
        )
        .map_err(|error| JsRuntimeError::SpawnFailed(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| JsRuntimeError::SpawnFailed("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| JsRuntimeError::SpawnFailed("no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| JsRuntimeError::SpawnFailed("no stderr".into()))?;

        let channel = Arc::new(RpcChannel::new(stdin, max_frame_bytes));
        let (sender, incoming) = mpsc::channel(256);
        let stdout_task = spawn_stdout_reader(stdout, Arc::clone(&channel), sender);
        let stderr_bytes = Arc::new(AtomicUsize::new(0));
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = tokio::spawn({
            let stderr_bytes = Arc::clone(&stderr_bytes);
            let stderr_tail = Arc::clone(&stderr_tail);
            async move {
                let mut stderr = BufReader::new(stderr);
                let mut buffer = vec![0; STDERR_CHUNK_BYTES];
                while let Ok(bytes) = stderr.read(&mut buffer).await {
                    if bytes == 0 {
                        break;
                    }
                    stderr_bytes.fetch_add(bytes, Ordering::Relaxed);
                    if let Ok(mut tail) = stderr_tail.lock() {
                        tail.extend_from_slice(&buffer[..bytes]);
                        if tail.len() > STDERR_TAIL_BYTES {
                            let excess = tail.len() - STDERR_TAIL_BYTES;
                            tail.drain(..excess);
                        }
                    }
                    debug!(target: "js_runtime:stderr", bytes, "JavaScript stderr received");
                }
            }
        });

        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            channel,
            incoming: tokio::sync::Mutex::new(Some(incoming)),
            process_tree,
            stdout_task: tokio::sync::Mutex::new(Some(stdout_task)),
            stderr_task: tokio::sync::Mutex::new(Some(stderr_task)),
            stderr_bytes,
            stderr_tail,
        })
    }

    pub fn channel(&self) -> Arc<RpcChannel> {
        Arc::clone(&self.channel)
    }

    pub async fn take_incoming(&self) -> Option<mpsc::Receiver<IncomingMessage>> {
        self.incoming.lock().await.take()
    }

    pub(crate) async fn wait_for_exit(&self) -> Result<std::process::ExitStatus> {
        self.child.lock().await.wait().await.map_err(Into::into)
    }

    pub(crate) fn stderr_bytes(&self) -> usize {
        self.stderr_bytes.load(Ordering::Relaxed)
    }

    pub async fn kill(&self) -> Result<()> {
        self.terminate_and_wait("JavaScript process cancelled", Duration::from_millis(100))
            .await
            .map(|_| ())
    }

    pub async fn wait(&self) -> Result<std::process::ExitStatus> {
        let status = self.child.lock().await.wait().await?;
        self.channel.drain_pending("JavaScript process exited");
        self.join_readers().await;
        Ok(status)
    }

    pub fn stderr_tail(&self) -> Option<String> {
        let tail = self.stderr_tail.lock().ok()?;
        if tail.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&tail).into_owned())
        }
    }

    pub(crate) async fn terminate_and_wait(
        &self,
        reason: &'static str,
        grace: Duration,
    ) -> Result<std::process::ExitStatus> {
        self.channel.drain_pending(reason);
        let _ = self.child.lock().await.try_wait()?;
        self.process_tree
            .terminate(grace)
            .await
            .map_err(|error| JsRuntimeError::CleanupFailed(error.to_string()))?;
        let status = self.child.lock().await.wait().await?;
        self.join_readers().await;
        Ok(status)
    }

    async fn join_readers(&self) {
        if let Some(task) = self.stdout_task.lock().await.take() {
            let _ = task.await;
        }
        if let Some(task) = self.stderr_task.lock().await.take() {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_bounded_stderr_tail() {
        let payload = "x".repeat(STDERR_TAIL_BYTES + 1024);
        let script = format!("process.stderr.write('prefix-' + '{}');", payload);
        let host =
            JsExecutionHost::spawn(JsProcessSpec::new("node", vec!["-e".into(), script])).unwrap();

        host.wait().await.unwrap();
        let tail = host.stderr_tail().unwrap();
        assert!(tail.len() <= STDERR_TAIL_BYTES);
        assert!(!tail.contains("prefix-"));
        assert!(tail.ends_with('x'));
    }
}
