use std::io;
use std::os::unix::process::CommandExt;

pub(super) fn prepare(command: &mut std::process::Command) {
    let inherited_group = unsafe { libc::getpgrp() };
    let inherited_session = unsafe { libc::getsid(0) };
    command.process_group(inherited_group);
    unsafe {
        command.pre_exec(move || {
            if libc::getsid(0) != inherited_session {
                return Err(io::Error::from_raw_os_error(libc::EPERM));
            }
            if libc::getpgrp() == libc::getpid() && libc::setpgid(0, inherited_group) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
#[path = "unix_test.rs"]
mod tests;
