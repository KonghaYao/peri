//! OS subprocess ownership shared by shell, MCP and JavaScript transports.
//! A termination request is not completion; owners must wait for actual group/job exit.
//! Unix commands start in a new session without an inherited controlling terminal.
//! Stdio descriptors remain the caller's responsibility; session isolation is not FD isolation.
//! Unix descendants that deliberately leave the group are outside this ownership boundary.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::{Child, Command};

#[cfg(unix)]
mod broker;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows_job;
#[cfg(windows)]
use windows_job as windows;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn leader_exit_does_not_settle_redirected_descendants() {
        let cwd = tempfile::tempdir().unwrap();
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                "sleep 60 >/dev/null 2>&1 & printf '%s' $! > descendant",
            ])
            .current_dir(cwd.path());
        let mut tree = ProcessTree::try_new().unwrap();
        tree.prepare(&mut command);
        let mut child = command.spawn().unwrap();
        tree.attach(&child).unwrap();
        assert!(child.wait().await.unwrap().success());
        assert!(cwd.path().join("descendant").is_file());
        assert!(!tree.is_stopped());
        assert!(
            peri_time::timeout(Duration::from_millis(20), tree.wait_for_exit())
                .await
                .is_err()
        );
        tree.terminate();
        peri_time::timeout(Duration::from_secs(5), tree.wait_for_exit())
            .await
            .unwrap();
        assert!(tree.is_stopped());
        tree.terminate();
        assert!(tree.is_stopped());
    }

    #[tokio::test]
    async fn cancelled_exit_wait_preserves_owner_for_retry() {
        let mut command = Command::new("sleep");
        command.arg("60");
        let mut tree = ProcessTree::try_new().unwrap();
        tree.prepare(&mut command);
        let mut child = command.spawn().unwrap();
        tree.attach(&child).unwrap();
        assert!(
            peri_time::timeout(Duration::from_millis(20), tree.wait_for_exit())
                .await
                .is_err()
        );
        assert!(!tree.is_stopped());
        tree.terminate();
        child.wait().await.unwrap();
        peri_time::timeout(Duration::from_secs(5), tree.wait_for_exit())
            .await
            .unwrap();
    }
}

mod process_tree;
pub use process_tree::run_output;
#[cfg(unix)]
pub use process_tree::run_output_blocking;
pub use process_tree::ProcessTree;
