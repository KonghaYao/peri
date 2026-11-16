#[cfg(unix)]
use super::broker;
#[cfg(unix)]
use super::unix;
#[cfg(windows)]
use super::windows;
use super::{io, AtomicBool, Child, Command, Ordering};
use std::fmt;
use std::fmt::{Debug, Formatter};

/// One dedicated Unix session/process group or Windows Job, prepared before child execution.
/// Keep this owner until `wait_for_exit` completes; Drop only requests termination.
/// This is a lifecycle primitive, not a sandbox preventing Unix setsid/setpgid.
pub struct ProcessTree {
    pub(super) attempted: bool,
    pub(super) pid: Option<u32>,
    pub(super) settled: AtomicBool,
    pub(super) terminate_on_drop: bool,
    #[cfg(unix)]
    pub(super) broker: Option<broker::Registration>,
    #[cfg(windows)]
    pub(super) job: windows::WindowsJob,
}

impl Debug for ProcessTree {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessTree")
            .field("attempted", &self.attempted)
            .field("pid", &self.pid)
            .field("settled", &self.settled)
            .field("terminate_on_drop", &self.terminate_on_drop)
            .finish_non_exhaustive()
    }
}

impl ProcessTree {
    /// Reserves the platform process owner before spawning a child.
    ///
    /// Unix broker registration and Windows Job creation may fail; callers must
    /// retain a successful owner through child termination and exit confirmation.
    pub fn try_new() -> io::Result<Self> {
        Ok(Self {
            attempted: false,
            pid: None,
            settled: AtomicBool::new(false),
            terminate_on_drop: true,
            #[cfg(unix)]
            broker: broker::Registration::from_environment()?,
            #[cfg(windows)]
            job: windows::WindowsJob::try_new()?,
        })
    }

    /// Prepare non-interactive execution without changing the caller's stdio configuration.
    /// Unix session isolation precedes broker registration and preserves a dedicated PGID.
    /// Call once per command: repeated preparation is not idempotent and fails at spawn on Unix.
    /// Use a new owner for each attached process tree.
    pub fn prepare(&self, command: &mut Command) {
        command.kill_on_drop(true);
        #[cfg(unix)]
        self.prepare_std(command.as_std_mut());
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }

    /// Prepare a synchronous Unix subprocess through the same broker protocol.
    /// Session isolation, stdio ownership and single-preparation rules match [`Self::prepare`].
    #[cfg(unix)]
    pub fn prepare_std(&self, command: &mut std::process::Command) {
        unix::prepare(command);
        if let Some(broker) = &self.broker {
            broker.prepare_std(command);
        }
    }

    #[cfg(unix)]
    pub fn attach_pid(&mut self, pid: u32) -> io::Result<()> {
        if self.attempted {
            return Err(io::Error::other("process tree already attached"));
        }
        self.attempted = true;
        self.pid = Some(pid);
        if pid > 0 && i32::try_from(pid).is_ok() {
            Ok(())
        } else {
            Err(io::Error::other("child process group unavailable"))
        }
    }

    #[cfg(unix)]
    pub fn wait_for_exit_blocking(&self) {
        while !self.is_stopped() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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

    #[must_use]
    pub fn is_stopped(&self) -> bool {
        if !self.attempted || self.settled.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(windows)]
        let stopped = self.job.is_stopped();
        #[cfg(unix)]
        let stopped = self.pid.is_some_and(|pid| {
            // Dedicated group created by prepare; ESRCH proves no member remains.
            // SAFETY: The checked nonzero process-group ID belongs to this execution; the syscall uses no pointers and errors are handled by the caller.
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
            // SAFETY: The checked nonzero process-group ID belongs to this execution; the syscall uses no pointers and errors are handled by the caller.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }

    pub async fn wait_for_exit(&self) {
        while !self.is_stopped() {
            peri_time::sleep(std::time::Duration::from_millis(20)).await;
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

/// Run a short-lived command under the same process-group ownership as tools.
pub async fn run_output(mut command: Command) -> io::Result<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut tree = ProcessTree::try_new()?;
    tree.prepare(&mut command);
    let mut child = command.spawn()?;
    if let Err(error) = tree.attach(&child) {
        tree.terminate();
        let _ = child.start_kill();
        return Err(error);
    }
    let output = child.wait_with_output().await;
    if output.is_err() {
        tree.terminate();
    }
    tree.wait_for_exit().await;
    output
}

/// Blocking variant for Git checks already running in a blocking worker.
#[cfg(unix)]
pub fn run_output_blocking(mut command: std::process::Command) -> io::Result<std::process::Output> {
    use std::process::Stdio;
    let mut tree = ProcessTree::try_new()?;
    tree.prepare_std(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn()?;
    tree.attach_pid(child.id())?;
    let output = child.wait_with_output();
    if output.is_err() {
        tree.terminate();
    }
    tree.wait_for_exit_blocking();
    output
}
