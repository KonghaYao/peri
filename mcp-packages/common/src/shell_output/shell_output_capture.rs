use super::AsyncWriteExt as _;
use super::{fmt, Arc, Debug, Formatter, Mutex, PathBuf, ShellOutput};

#[derive(Debug, Default)]
pub(super) struct StreamState {
    pub(super) path: Option<String>,
    pub(super) error: Option<String>,
    pub(super) finished: bool,
}

#[derive(Debug, Default)]
pub(super) struct CaptureState {
    pub(super) stdout: StreamState,
    pub(super) stderr: StreamState,
    pub(super) read_error: Option<String>,
    pub(super) retained: bool,
}

pub(super) struct UnpublishedFiles(pub(super) [Option<String>; 2]);

impl Drop for UnpublishedFiles {
    fn drop(&mut self) {
        {
            let iterated_entries = self.0.iter().flatten();
            for path in iterated_entries {
                if let Err(error) = std::fs::remove_file(path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(%path, %error, "unpublished shell output cleanup failed");
                    }
                }
            }
        }
    }
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        let paths = [self.stdout.path.take(), self.stderr.path.take()];
        if paths.iter().all(Option::is_none) {
            return;
        }
        // The last capture/writer owns cleanup, including cancellation while
        // the constructor is still on the blocking pool. Never unlink while
        // another writer can still be using the file.
        // The closure owns a cleanup guard so even rejection by a closed
        // runtime removes the files when the unstarted closure is dropped.
        let cleanup = UnpublishedFiles(paths);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || drop(cleanup));
        } else {
            drop(cleanup);
        }
    }
}

/// Owns the files used by one shell execution. Files are created before the
/// pipe readers are spawned, so timeout promotion never has to reconstruct
/// output from the bounded preview buffer.
pub struct ShellOutputCapture {
    pub(super) state: Arc<Mutex<CaptureState>>,
    pub(super) stdout_file: Option<OutputFile>,
    pub(super) stderr_file: Option<OutputFile>,
}

impl Debug for ShellOutputCapture {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShellOutputCapture")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(super) struct OutputFile {
    pub(super) file: tokio::fs::File,
    pub(super) state: Arc<Mutex<CaptureState>>,
    pub(super) stdout: bool,
}

/// A writer passed to a pipe-drain task. The write error is retained for the
/// eventual typed result instead of being silently discarded.
#[derive(Debug)]
pub struct ShellOutputWriter(pub(super) Option<OutputFile>);

impl ShellOutputCapture {
    /// Create the two output files. Callers should invoke this small
    /// synchronous setup from a blocking context. Stream writes and explicit
    /// cleanup use Tokio's async file APIs; drop cleanup uses its blocking pool
    /// while a runtime is available.
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        let state = Arc::new(Mutex::new(CaptureState::default()));
        let stdout_file = Self::open_stream(&state, prefix, "stdout", true);
        let stderr_file = Self::open_stream(&state, prefix, "stderr", false);
        Self {
            state,
            stdout_file,
            stderr_file,
        }
    }

    pub(super) fn open_stream(
        state: &Arc<Mutex<CaptureState>>,
        prefix: &str,
        stream: &str,
        stdout: bool,
    ) -> Option<OutputFile> {
        let mut path = std::env::temp_dir();
        if !path.is_absolute() {
            path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
        path.push(format!(
            "peri-{prefix}-{}-{stream}.log",
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                let path = path.to_string_lossy().into_owned();
                if let Ok(mut guard) = state.lock() {
                    if stdout {
                        guard.stdout.path = Some(path);
                    } else {
                        guard.stderr.path = Some(path);
                    }
                }
                Some(OutputFile {
                    file: tokio::fs::File::from_std(file),
                    state: Arc::clone(state),
                    stdout,
                })
            }
            Err(error) => {
                Self::record_stream_error(
                    state,
                    stdout,
                    format!("create {stream} output file: {error}"),
                );
                None
            }
        }
    }

    pub fn stdout_writer(&mut self) -> ShellOutputWriter {
        ShellOutputWriter(self.stdout_file.take())
    }

    pub fn stdout_path(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|guard| guard.stdout.path.clone())
    }

    pub fn stderr_path(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|guard| guard.stderr.path.clone())
    }

    pub fn stderr_writer(&mut self) -> ShellOutputWriter {
        ShellOutputWriter(self.stderr_file.take())
    }

    /// Preserve paths handed to an accepted background task for later Read.
    /// Unpublished captures are otherwise removed when their last owner drops.
    pub fn retain_files(&self) {
        self.state
            .lock()
            .expect("shell output capture must remain consistent after a panic")
            .retained = true;
    }

    /// Record a pipe read failure. EOF is represented by `Ok(0)` and is not an
    /// error; callers should invoke this only for an actual read error.
    pub fn record_read_error(&self, stream: &str, error: impl std::fmt::Display) {
        if let Ok(mut guard) = self.state.lock() {
            guard.read_error = Some(format!("read {stream} output pipe: {error}"));
        }
    }

    pub fn record_task_error(&self, stream: &str, error: impl std::fmt::Display) {
        if let Ok(mut guard) = self.state.lock() {
            guard.read_error = Some(format!("{stream} output reader failed: {error}"));
        }
    }

    pub fn mark_incomplete(&self, reason: impl std::fmt::Display) {
        if let Ok(mut guard) = self.state.lock() {
            guard.read_error = Some(format!("output capture incomplete: {reason}"));
        }
    }

    pub fn finish(&self, exit_code: Option<i32>) -> ShellOutput {
        let guard = self
            .state
            .lock()
            .expect("shell output capture must remain consistent after a panic");
        let mut errors = Vec::new();
        if let Some(error) = &guard.stdout.error {
            errors.push(error.clone());
        }
        if let Some(error) = &guard.stderr.error {
            errors.push(error.clone());
        }
        if let Some(error) = &guard.read_error {
            errors.push(error.clone());
        }
        for (stream, state) in [("stdout", &guard.stdout), ("stderr", &guard.stderr)] {
            if state.path.is_some() && !state.finished && state.error.is_none() {
                errors.push(format!("{stream} output stream was not finalized"));
            }
        }
        ShellOutput {
            stdout_path: guard.stdout.path.clone(),
            stderr_path: guard.stderr.path.clone(),
            complete: errors.is_empty()
                && guard.stdout.path.is_some()
                && guard.stderr.path.is_some()
                && guard.stdout.finished
                && guard.stderr.finished,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            exit_code,
        }
    }

    /// Remove unreferenced files after a normal foreground command. Promoted
    /// and explicit background paths retain their files for Read.
    pub async fn cleanup(&self) {
        let paths = self
            .state
            .lock()
            .ok()
            .map(|guard| {
                [guard.stdout.path.clone(), guard.stderr.path.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for path in paths {
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    pub(super) fn record_stream_error(
        state: &Arc<Mutex<CaptureState>>,
        stdout: bool,
        error: String,
    ) {
        if let Ok(mut guard) = state.lock() {
            let target = if stdout {
                &mut guard.stdout
            } else {
                &mut guard.stderr
            };
            target.error = Some(error);
        }
    }
}

impl ShellOutputWriter {
    pub async fn write_chunk(&mut self, bytes: &[u8]) {
        let Some(output) = self.0.as_mut() else {
            return;
        };
        if let Err(error) = output.file.write_all(bytes).await {
            ShellOutputCapture::record_stream_error(
                &output.state,
                output.stdout,
                format!(
                    "write {} output file: {error}",
                    if output.stdout { "stdout" } else { "stderr" }
                ),
            );
            self.0 = None;
        }
    }

    pub async fn finish(&mut self) {
        let Some(output) = self.0.as_mut() else {
            return;
        };
        if let Err(error) = output.file.flush().await {
            ShellOutputCapture::record_stream_error(
                &output.state,
                output.stdout,
                format!(
                    "flush {} output file: {error}",
                    if output.stdout { "stdout" } else { "stderr" }
                ),
            );
            self.0 = None;
        } else if let Ok(mut guard) = output.state.lock() {
            let stream = if output.stdout {
                &mut guard.stdout
            } else {
                &mut guard.stderr
            };
            stream.finished = true;
        }
    }
}
