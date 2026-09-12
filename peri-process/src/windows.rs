//! Windows shell ownership. Assignment happens while the primary thread is suspended.

use std::{
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            GetProcessId, GetProcessIdOfThread, OpenThread, ResumeThread, TerminateProcess,
            WaitForSingleObject, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
        },
    },
};

pub(super) struct WindowsJob {
    handle: OwnedHandle,
    process: OnceLock<OwnedHandle>,
    attachment_attempted: AtomicBool,
}

impl WindowsJob {
    pub(super) fn new() -> io::Result<Self> {
        // Null security attributes create a non-inheritable unnamed job handle.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // The handle was just created and its sole ownership transfers into OwnedHandle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // The typed structure is initialized and remains valid for the synchronous API.
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            process: OnceLock::new(),
            attachment_attempted: AtomicBool::new(false),
        })
    }

    fn retain_process(&self, child: &tokio::process::Child) -> io::Result<u32> {
        self.attachment_attempted.store(true, Ordering::Release);
        let pid = child
            .id()
            .ok_or_else(|| invalid_process("shell exited before job assignment"))?;
        let process = child
            .raw_handle()
            .ok_or_else(|| invalid_process("shell handle is unavailable"))?;
        // Clone while Child keeps the borrowed handle valid. Even assignment failure
        // leaves an owned suspended leader; an empty job alone cannot prove cleanup.
        let process = unsafe { BorrowedHandle::borrow_raw(process) }.try_clone_to_owned()?;
        self.process
            .set(process)
            .map_err(|_| invalid_process("shell process was already attached"))?;
        let process = self
            .process
            .get()
            .expect("retained process")
            .as_raw_handle();
        // Verify identity against the retained process handle, not only a reusable PID.
        if unsafe { GetProcessId(process) } != pid {
            return Err(invalid_process("shell process identity changed"));
        }
        Ok(pid)
    }

    /// The caller must spawn with CREATE_SUSPENDED, then kill and wait on any error.
    pub(super) fn attach_and_resume(&self, child: &tokio::process::Child) -> io::Result<()> {
        let pid = self.retain_process(child)?;
        let process = self
            .process
            .get()
            .expect("retained process")
            .as_raw_handle();
        let primary = suspended_primary_thread(pid)?;
        // The shell has not executed user code. Descendants therefore cannot escape
        // between spawn and assignment, and this job disallows breakaway creation.
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let previous_suspend_count = unsafe { ResumeThread(primary.as_raw_handle()) };
        match previous_suspend_count {
            1 => Ok(()),
            u32::MAX => Err(io::Error::last_os_error()),
            _ => Err(invalid_process(
                "shell primary thread was not suspended exactly once",
            )),
        }
    }

    pub(super) fn is_stopped(&self) -> bool {
        if self.attachment_attempted.load(Ordering::Acquire)
            && !self.process.get().is_some_and(|process| {
                // Zero timeout probes the retained process object without waiting or PID lookup.
                unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
            })
        {
            return false;
        }
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // Query failure is unknown cleanup state, never evidence of an empty process tree.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        };
        queried != 0 && accounting.ActiveProcesses == 0
    }

    pub(super) fn disarm(&self) -> io::Result<()> {
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let configured = unsafe {
            SetInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn terminate(&self) {
        // Termination is asynchronous; only a later is_stopped query establishes clean.
        unsafe {
            TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
        if let Some(process) = self.process.get() {
            // Also cover failure before assignment. The command is still suspended then.
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
            }
        }
    }
}

fn invalid_process(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn suspended_primary_thread(pid: u32) -> io::Result<OwnedHandle> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // ToolHelp returns a new non-inherited snapshot handle with sole ownership here.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut primary = None;
    loop {
        if entry.th32OwnerProcessID == pid {
            if primary.is_some() {
                return Err(invalid_process("suspended shell has multiple threads"));
            }
            let thread = unsafe {
                OpenThread(
                    THREAD_QUERY_LIMITED_INFORMATION | THREAD_SUSPEND_RESUME,
                    0,
                    entry.th32ThreadID,
                )
            };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // A snapshot thread ID may have disappeared or been reused before OpenThread.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
                return Err(invalid_process("shell thread identity changed"));
            }
            primary = Some(thread);
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
    }
    primary.ok_or_else(|| invalid_process("suspended shell primary thread is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_empty_job_does_not_settle_unassigned_suspended_leader() {
        let job = WindowsJob::new().unwrap();
        let mut command = tokio::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        command
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        job.retain_process(&child).unwrap();
        assert!(!job.is_stopped());
        job.terminate();
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(job.is_stopped());
    }

    #[tokio::test]
    async fn test_windows_job_owns_shell_until_process_termination() {
        let job = WindowsJob::new().unwrap();
        let mut command = tokio::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        command
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        job.attach_and_resume(&child).unwrap();
        assert!(!job.is_stopped());
        job.terminate();
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(job.is_stopped());
    }
}
