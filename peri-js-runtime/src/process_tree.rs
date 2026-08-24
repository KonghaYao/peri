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
pub(crate) struct ProcessTree {
    job: isize,
}

#[cfg(windows)]
impl ProcessTree {
    pub(crate) fn new(process: windows_sys::Win32::Foundation::HANDLE) -> io::Result<Self> {
        use std::{mem::size_of, ptr};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: null security attributes and name request an unnamed job with default security.
        let job = unsafe {
            windows_sys::Win32::System::JobObjects::CreateJobObjectW(ptr::null(), ptr::null())
        };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: job is owned and valid; limits points to the correctly sized information type.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: job was created above and has not been closed.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(error);
        }
        // SAFETY: job and process are valid handles owned by the host and spawned child.
        let assigned = unsafe { AssignProcessToJobObject(job, process) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: job was created above and has not been closed.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(error);
        }
        Ok(Self { job: job as isize })
    }

    pub(crate) async fn terminate(&self, _grace: std::time::Duration) -> io::Result<()> {
        let job = self.job as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: job remains owned by self until Drop and is not closed concurrently.
        if unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(job, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let job = self.job as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: self uniquely owns the job handle and Drop runs once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
    }
}
