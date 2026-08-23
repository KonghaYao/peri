use std::io;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct ProcessTree {
    pgid: i32,
    converged: AtomicBool,
}

#[cfg(unix)]
impl ProcessTree {
    pub(crate) fn new(child_id: u32) -> io::Result<Self> {
        let pgid = i32::try_from(child_id)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "child pid exceeds i32"))?;
        if pgid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid child process group",
            ));
        }
        Ok(Self {
            pgid,
            converged: AtomicBool::new(false),
        })
    }

    pub(crate) async fn terminate(&self, _grace: Duration) -> io::Result<()> {
        if self.converged.load(Ordering::Acquire) {
            return Ok(());
        }
        self.signal(libc::SIGTERM)?;
        self.signal(libc::SIGKILL)?;
        self.converged.store(true, Ordering::Release);
        Ok(())
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        // SAFETY: the host creates a dedicated positive process group for this child.
        let result = unsafe { libc::kill(-self.pgid, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.converged.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: this is a best-effort fail-safe for the dedicated child process group.
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct ProcessTree;

#[cfg(windows)]
impl ProcessTree {
    pub(crate) fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "process-tree containment is unavailable on this build",
        )
    }

    pub(crate) async fn terminate(&self, _grace: std::time::Duration) -> io::Result<()> {
        Err(Self::unsupported())
    }
}
