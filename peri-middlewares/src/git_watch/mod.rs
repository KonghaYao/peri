//! Git Watch：异步采样 branch + HEAD，变化时注入 Info（不监视 working tree）。

mod snapshot;

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    error::AgentResult,
    messages::BaseMessage,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    session::{MessageKind, MessageSource, QueuedMessage},
};
use snapshot::{
    info_message_if_changed, parse_sample_stdout, GitSnapshot, SampleOutcome,
    GIT_WATCH_SAMPLE_TIMEOUT, GIT_WATCH_THROTTLE,
};
use tokio::process::Command;

pub use snapshot::GitSnapshot as GitWatchSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoMode {
    Unknown,
    NotRepository,
    Repository,
}

struct GitWatchInner {
    repo_mode: Mutex<RepoMode>,
    last_snapshot: Mutex<Option<GitSnapshot>>,
    /// 上次采样**完成**时刻（用于 60s 节流，Q4）。
    last_sample_completed_at: Mutex<Option<Instant>>,
    in_flight: AtomicBool,
    throttle: Duration,
    sample_timeout: Duration,
}

/// 监视 git 分支与 HEAD；**成功**工具返回后触发采样（节流 + 后台），不监视 working tree。
pub struct GitWatchMiddleware {
    inner: Arc<GitWatchInner>,
}

impl Default for GitWatchMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl GitWatchMiddleware {
    pub fn new() -> Self {
        Self::with_timing(GIT_WATCH_THROTTLE, GIT_WATCH_SAMPLE_TIMEOUT)
    }

    fn with_timing(throttle: Duration, sample_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(GitWatchInner {
                repo_mode: Mutex::new(RepoMode::Unknown),
                last_snapshot: Mutex::new(None),
                last_sample_completed_at: Mutex::new(None),
                in_flight: AtomicBool::new(false),
                throttle,
                sample_timeout,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_throttle_for_test(throttle: Duration) -> Self {
        Self::with_timing(throttle, GIT_WATCH_SAMPLE_TIMEOUT)
    }

    fn schedule_sample(&self, state: &dyn MiddlewareState) {
        if matches!(
            *self
                .inner
                .repo_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            RepoMode::NotRepository
        ) {
            return;
        }

        if self.inner.in_flight.load(Ordering::Acquire) {
            return;
        }

        if let Ok(guard) = self.inner.last_sample_completed_at.lock() {
            if let Some(completed) = *guard {
                if completed.elapsed() < self.inner.throttle {
                    return;
                }
            }
        }

        if self
            .inner
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let cwd = state.cwd().to_string();
        let queue = state.v2_queue().clone();
        let inner = Arc::clone(&self.inner);

        tokio::spawn(async move {
            let finish = || {
                inner.in_flight.store(false, Ordering::Release);
            };

            let outcome =
                match tokio::time::timeout(inner.sample_timeout, run_git_sample(&cwd)).await {
                    Ok(o) => o,
                    Err(_) => {
                        tracing::warn!(
                            target: "git_watch",
                            cwd = %cwd,
                            timeout_ms = inner.sample_timeout.as_millis(),
                            "git sample timed out"
                        );
                        finish();
                        return;
                    }
                };

            match outcome {
                SampleOutcome::Failed => {
                    tracing::debug!(target: "git_watch", cwd = %cwd, "git sample failed");
                    finish();
                    return;
                }
                SampleOutcome::NotRepository => {
                    if let Ok(mut mode) = inner.repo_mode.lock() {
                        *mode = RepoMode::NotRepository;
                    }
                    finish();
                    return;
                }
                SampleOutcome::Repository(current) => {
                    if let Ok(mut mode) = inner.repo_mode.lock() {
                        *mode = RepoMode::Repository;
                    }

                    let notify = {
                        let mut snap_guard = inner
                            .last_snapshot
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let previous = snap_guard.clone();
                        let msg = info_message_if_changed(previous.as_ref(), &current);
                        *snap_guard = Some(current);
                        msg
                    };

                    if let Some(text) = notify {
                        queue.push(QueuedMessage::new(
                            MessageKind::Info,
                            MessageSource::SystemInjected,
                            BaseMessage::human(text),
                        ));
                    }

                    if let Ok(mut completed) = inner.last_sample_completed_at.lock() {
                        *completed = Some(Instant::now());
                    }
                }
            }

            finish();
        });
    }
}

async fn run_git_sample(cwd: &str) -> SampleOutcome {
    let output = match Command::new("git")
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "rev-parse",
            "--is-inside-work-tree",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return SampleOutcome::Failed,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let work_tree = lines.next().unwrap_or("").trim();
    if work_tree != "true" {
        if work_tree == "false" {
            return SampleOutcome::NotRepository;
        }
        return SampleOutcome::Failed;
    }
    let head = lines.next().unwrap_or("").trim();
    let branch = lines.next().unwrap_or("").trim();
    if head.is_empty() || branch.is_empty() {
        return SampleOutcome::Failed;
    }

    parse_sample_stdout(&format!("true\n{head}\n{branch}\n"))
}

#[async_trait]
impl Middleware for GitWatchMiddleware {
    fn name(&self) -> &str {
        "GitWatchMiddleware"
    }

    async fn after_tool(
        &self,
        state: &mut dyn MiddlewareState,
        _tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        if !result.is_error {
            self.schedule_sample(state);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
