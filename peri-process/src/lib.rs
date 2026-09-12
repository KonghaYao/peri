//! OS subprocess ownership shared by shell, MCP, LSP and JavaScript transports.
//! A termination request is not completion; owners must wait for actual group/job exit.
//! Unix descendants that deliberately leave the group are outside this ownership boundary.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::{Child, Command};

#[cfg(windows)]
mod windows;

/// One dedicated Unix process group or Windows Job, prepared before child execution.
/// Keep this owner until `wait_for_exit` completes; Drop only requests termination.
/// This is a lifecycle primitive, not a sandbox preventing Unix setsid/setpgid.
pub struct ProcessTree {
    attempted: bool,
    pid: Option<u32>,
    settled: AtomicBool,
    terminate_on_drop: bool,
    #[cfg(windows)]
    job: windows::WindowsJob,
}

impl ProcessTree {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            attempted: false,
            pid: None,
            settled: AtomicBool::new(false),
            terminate_on_drop: true,
            #[cfg(windows)]
            job: windows::WindowsJob::new()?,
        })
    }

    pub fn prepare(&self, command: &mut Command) {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }

    /// Call immediately after spawning the prepared command, before publishing its handle.
    /// On error retain the owner while killing/reaping the child; cleanup may remain unknown.
    pub fn attach(&mut self, child: &Child) -> io::Result<()> {
        if self.attempted {
            return Err(io::Error::other("process tree already attached"));
        }
        self.attempted = true;
        self.pid = child.id();
        #[cfg(windows)]
        return self.job.attach_and_resume(child);
        #[cfg(not(windows))]
        match self.pid {
            Some(pid) if pid > 0 && i32::try_from(pid).is_ok() => Ok(()),
            _ => Err(io::Error::other("child process group unavailable")),
        }
    }

    pub fn is_stopped(&self) -> bool {
        if !self.attempted || self.settled.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(windows)]
        let stopped = self.job.is_stopped();
        #[cfg(unix)]
        let stopped = self.pid.is_some_and(|pid| {
            // Dedicated group created by prepare; ESRCH proves no member remains.
            let result = unsafe { libc::kill(-(pid as i32), 0) };
            result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        #[cfg(not(any(unix, windows)))]
        let stopped = false;
        if stopped {
            // Once absence is observed, never probe or signal a subsequently reused group ID.
            self.settled.store(true, Ordering::Release);
        }
        stopped
    }

    /// Request whole-tree termination; errors are reflected by unconfirmed exit.
    pub fn terminate(&self) {
        if self.is_stopped() {
            return;
        }
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The same dedicated group remains owned until absence has been observed.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }

    pub async fn wait_for_exit(&self) {
        while !self.is_stopped() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Explicitly transfer unmanaged process lifetime to a standalone caller.
    /// Session-owned execution must never use this escape hatch.
    pub fn disarm(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        self.job.disarm()?;
        self.terminate_on_drop = false;
        Ok(())
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.terminate_on_drop {
            self.terminate();
        }
    }
}

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
        let mut tree = ProcessTree::new().unwrap();
        tree.prepare(&mut command);
        let mut child = command.spawn().unwrap();
        tree.attach(&child).unwrap();
        assert!(child.wait().await.unwrap().success());
        assert!(cwd.path().join("descendant").is_file());
        assert!(!tree.is_stopped());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), tree.wait_for_exit())
                .await
                .is_err()
        );
        tree.terminate();
        tokio::time::timeout(Duration::from_secs(5), tree.wait_for_exit())
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
        let mut tree = ProcessTree::new().unwrap();
        tree.prepare(&mut command);
        let mut child = command.spawn().unwrap();
        tree.attach(&child).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), tree.wait_for_exit())
                .await
                .is_err()
        );
        assert!(!tree.is_stopped());
        tree.terminate();
        child.wait().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), tree.wait_for_exit())
            .await
            .unwrap();
    }
}
