//! Windows shell ownership. Assignment happens while the primary thread is suspended.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::BorrowedHandle;
use std::os::windows::io::FromRawHandle;
use std::os::windows::io::OwnedHandle;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Thread32First;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Thread32Next;
use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPTHREAD;
use windows_sys::Win32::System::Diagnostics::ToolHelp::THREADENTRY32;
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
use windows_sys::Win32::System::JobObjects::JobObjectBasicAccountingInformation;
use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;
use windows_sys::Win32::System::JobObjects::SetInformationJobObject;
use windows_sys::Win32::System::JobObjects::TerminateJobObject;
use windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION;
use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
use windows_sys::Win32::System::Threading::GetProcessId;
use windows_sys::Win32::System::Threading::GetProcessIdOfThread;
use windows_sys::Win32::System::Threading::OpenThread;
use windows_sys::Win32::System::Threading::ResumeThread;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::System::Threading::THREAD_QUERY_LIMITED_INFORMATION;
use windows_sys::Win32::System::Threading::THREAD_SUSPEND_RESUME;

pub(super) struct WindowsJob {
    handle: OwnedHandle,
    process: OnceLock<OwnedHandle>,
    attachment_attempted: AtomicBool,
}

impl WindowsJob {
    pub(super) fn try_new() -> io::Result<Self> {
        // Null security attributes create a non-inheritable unnamed job handle.
        // SAFETY: The null attributes request a non-inheritable handle; both optional name pointers are null.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // The handle was just created and its sole ownership transfers into OwnedHandle.
        // SAFETY: The preceding Win32 call returned a fresh valid handle, and this is its only ownership transfer.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // The typed structure is initialized and remains valid for the synchronous API.
        // SAFETY: The owned job handle is live and the initialized limits structure and its exact size remain valid for this call.
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
        // SAFETY: Child keeps the process handle live while it is borrowed and cloned into an independently owned handle.
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
        // SAFETY: The retained owned process handle remains live for this identity query.
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
        // SAFETY: Both handles are owned and live; the process was spawned suspended and has not executed user code.
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The owned primary-thread handle is live and was located from the suspended child process.
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
                // SAFETY: The retained owned process handle remains live; a zero timeout only probes its signal state.
                unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
            })
        {
            return false;
        }
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // Query failure is unknown cleanup state, never evidence of an empty process tree.
        // SAFETY: The live job handle and initialized output structure match the information class and exact buffer size.
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
        // SAFETY: The owned job handle is live and the initialized limits structure and its exact size remain valid for this call.
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
        // SAFETY: The owned job handle remains live; termination does not transfer or close it.
        unsafe {
            TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
        if let Some(process) = self.process.get() {
            // Also cover failure before assignment. The command is still suspended then.
            // SAFETY: The retained owned process handle remains live and identifies the child even if job assignment failed.
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
    // SAFETY: The snapshot request uses valid flags and needs no pointers; failure is checked before owning the result.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // ToolHelp returns a new non-inherited snapshot handle with sole ownership here.
    // SAFETY: The preceding Win32 call returned a fresh valid handle, and this is its only ownership transfer.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: The snapshot handle is live and entry is initialized with the required structure size.
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut primary = None;
    loop {
        if entry.th32OwnerProcessID == pid {
            if primary.is_some() {
                return Err(invalid_process("suspended shell has multiple threads"));
            }
            // SAFETY: The snapshot supplies a thread ID; the returned handle is checked and its process identity is verified before use.
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
            // SAFETY: The preceding Win32 call returned a fresh valid handle, and this is its only ownership transfer.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // A snapshot thread ID may have disappeared or been reused before OpenThread.
            // SAFETY: The owned thread handle remains live for this identity query.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
                return Err(invalid_process("shell thread identity changed"));
            }
            primary = Some(thread);
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        // SAFETY: The snapshot handle is live and entry size is reset before the API writes the next record.
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
        let job = WindowsJob::try_new().unwrap();
        let mut command = tokio::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        command
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        job.retain_process(&child).unwrap();
        assert!(!job.is_stopped());
        job.terminate();
        peri_time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(job.is_stopped());
    }

    #[tokio::test]
    async fn test_windows_job_owns_shell_until_process_termination() {
        let job = WindowsJob::try_new().unwrap();
        let mut command = tokio::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        command
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        job.attach_and_resume(&child).unwrap();
        assert!(!job.is_stopped());
        job.terminate();
        peri_time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(job.is_stopped());
    }
}
