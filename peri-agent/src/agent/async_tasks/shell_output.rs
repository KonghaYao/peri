//! Durable stdout/stderr capture for shell tasks.
//!
//! The in-memory preview is deliberately bounded, but a promoted shell must
//! continue writing its original pipes to disk from the moment it starts.
//! This module owns the side effects and returns only typed evidence to the
//! task result.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use peri_acp_types::event::ShellOutput;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Default)]
struct StreamState {
    path: Option<String>,
    error: Option<String>,
    finished: bool,
}

#[derive(Debug, Default)]
struct CaptureState {
    stdout: StreamState,
    stderr: StreamState,
    read_error: Option<String>,
}

/// Owns the files used by one shell execution. Files are created before the
/// pipe readers are spawned, so timeout promotion never has to reconstruct
/// output from the bounded preview buffer.
pub struct ShellOutputCapture {
    state: Arc<Mutex<CaptureState>>,
    stdout_file: Option<OutputFile>,
    stderr_file: Option<OutputFile>,
}

struct OutputFile {
    file: tokio::fs::File,
    state: Arc<Mutex<CaptureState>>,
    stdout: bool,
}

/// A writer passed to a pipe-drain task. The write error is retained for the
/// eventual typed result instead of being silently discarded.
pub struct ShellOutputWriter(Option<OutputFile>);

impl ShellOutputCapture {
    /// Create the two output files. Callers should invoke this small
    /// synchronous setup from a blocking context; all subsequent stream
    /// writes and cleanup use Tokio's async file APIs.
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

    fn open_stream(
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
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

    fn record_stream_error(state: &Arc<Mutex<CaptureState>>, stdout: bool, error: String) {
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

#[cfg(test)]
mod tests {
    use super::ShellOutputCapture;

    #[tokio::test]
    async fn capture_preserves_unicode_and_large_tail() {
        let mut capture = ShellOutputCapture::new("shell-output-test");
        let stdout_path = capture.stdout_path().expect("stdout file");
        let stderr_path = capture.stderr_path().expect("stderr file");
        let mut stdout = capture.stdout_writer();
        let mut stderr = capture.stderr_writer();
        let body = "前缀🙂".repeat(1024 * 512);
        stdout.write_chunk(body.as_bytes()).await;
        stderr.write_chunk("stderr-尾部\n".as_bytes()).await;
        stdout.finish().await;
        stderr.finish().await;

        let evidence = capture.finish(Some(7));
        assert!(evidence.complete);
        assert_eq!(evidence.exit_code, Some(7));
        assert_eq!(
            std::fs::read(&stdout_path).expect("stdout contents"),
            body.as_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(&stderr_path).expect("stderr contents"),
            "stderr-尾部\n"
        );
        capture.cleanup().await;
    }

    #[tokio::test]
    async fn finish_before_eof_is_incomplete() {
        let mut capture = ShellOutputCapture::new("shell-output-early-finish");
        let mut stdout = capture.stdout_writer();
        let mut stderr = capture.stderr_writer();
        stdout.write_chunk(b"partial").await;
        let early = capture.finish(None);
        assert!(!early.complete);
        assert!(early
            .error
            .as_deref()
            .is_some_and(|error| error.contains("not finalized")));
        stdout.finish().await;
        stderr.finish().await;
        assert!(capture.finish(None).complete);
        capture.cleanup().await;
    }

    #[tokio::test]
    async fn discarded_writer_is_reported_incomplete() {
        let mut capture = ShellOutputCapture::new("shell-output-discarded");
        let stdout = capture.stdout_writer();
        drop(stdout);
        let mut stderr = capture.stderr_writer();
        stderr.finish().await;
        let evidence = capture.finish(None);
        assert!(!evidence.complete);
        assert!(evidence
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stdout output stream was not finalized")));
        capture.cleanup().await;
    }

    #[test]
    fn creation_failure_is_explicit() {
        let prefix = format!("missing-output-parent-{}/stream", uuid::Uuid::new_v4());
        let capture = ShellOutputCapture::new(&prefix);
        let evidence = capture.finish(None);
        assert!(!evidence.complete);
        assert!(evidence.error.is_some());
        assert!(evidence.stdout_path.is_none());
        assert!(evidence.stderr_path.is_none());
    }

    #[tokio::test]
    async fn write_failure_is_explicit() {
        let mut capture = ShellOutputCapture::new("shell-output-write-failure");
        let mut stdout = capture.stdout_writer();
        let file =
            std::fs::File::open(capture.stdout_path().unwrap()).expect("open read-only file");
        stdout.0.as_mut().expect("stdout writer").file = tokio::fs::File::from_std(file);
        stdout
            .write_chunk(b"cannot write to a read-only file")
            .await;
        // Tokio may report the blocking write error from the next flush.
        stdout.finish().await;
        let mut stderr = capture.stderr_writer();
        stderr.finish().await;
        let evidence = capture.finish(None);
        assert!(!evidence.complete);
        assert!(evidence
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stdout output file")));
        capture.cleanup().await;
    }
}
