#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Child;
#[cfg(unix)]
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
#[cfg(unix)]
use tracing::debug;

use crate::process_tree::ProcessTree;
#[cfg(unix)]
use crate::rpc::spawn_stdout_reader;
use crate::{IncomingMessage, JsRuntimeError, Result, RpcChannel};

const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
#[cfg(unix)]
const STDERR_CHUNK_BYTES: usize = 8 * 1024;

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
}

impl JsExecutionHost {
    pub fn spawn(spec: JsProcessSpec) -> Result<Self> {
        Self::spawn_with_frame_limit(spec, DEFAULT_MAX_FRAME_BYTES)
    }

    pub(crate) fn spawn_with_frame_limit(
        spec: JsProcessSpec,
        max_frame_bytes: usize,
    ) -> Result<Self> {
        #[cfg(windows)]
        {
            let _ = (spec, max_frame_bytes);
            Err(JsRuntimeError::SpawnFailed(
                ProcessTree::unsupported().to_string(),
            ))
        }
        #[cfg(unix)]
        {
            Self::spawn_unix(spec, max_frame_bytes)
        }
    }

    #[cfg(unix)]
    fn spawn_unix(spec: JsProcessSpec, max_frame_bytes: usize) -> Result<Self> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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
        let child_id = child
            .id()
            .ok_or_else(|| JsRuntimeError::SpawnFailed("child pid unavailable".into()))?;
        let process_tree = ProcessTree::new(child_id)
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
        let stderr_task = tokio::spawn(async move {
            let mut stderr = BufReader::new(stderr);
            let mut buffer = vec![0; STDERR_CHUNK_BYTES];
            while let Ok(bytes) = stderr.read(&mut buffer).await {
                if bytes == 0 {
                    break;
                }
                debug!(target: "js_runtime:stderr", bytes, "JavaScript stderr received");
            }
        });

        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            channel,
            incoming: tokio::sync::Mutex::new(Some(incoming)),
            process_tree,
            stdout_task: tokio::sync::Mutex::new(Some(stdout_task)),
            stderr_task: tokio::sync::Mutex::new(Some(stderr_task)),
        })
    }

    pub fn channel(&self) -> Arc<RpcChannel> {
        Arc::clone(&self.channel)
    }

    pub async fn take_incoming(&self) -> Option<mpsc::Receiver<IncomingMessage>> {
        self.incoming.lock().await.take()
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
        None
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
