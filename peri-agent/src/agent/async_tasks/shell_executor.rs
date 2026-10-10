use std::sync::Arc;

use super::BackgroundTaskRegistry;
use peri_acp_types::tasks::{BgShellHandle, ExternalExecutionGuard, OnBgCompleteFn};

/// Execution-environment port; Agent holds session admission across synchronous startup.
/// Implementations must register before returning a published handle and retain cleanup
/// evidence until the process tree and pipe readers have stopped, including cancellation.
pub trait ShellExecutor: Send + Sync {
    fn cancel_callback(
        &self,
        pid: u32,
        registry: Arc<BackgroundTaskRegistry>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>>;

    fn spawn(
        &self,
        registry: Arc<BackgroundTaskRegistry>,
        ownership: Box<dyn ExternalExecutionGuard>,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>>;
}
