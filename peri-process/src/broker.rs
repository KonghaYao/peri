//! Child registration with the trusted stdio supervisor before exec.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::Duration;

use tokio::process::Command;

const SOCKET_ENV: &str = "PERI_PROCESS_BROKER_SOCKET";
const TOKEN_ENV: &str = "PERI_PROCESS_BROKER_TOKEN";
const ACK: u8 = 0x06;

pub(super) struct Registration {
    stream: UnixStream,
}

impl Registration {
    pub(super) fn from_environment() -> io::Result<Option<Self>> {
        let socket = std::env::var_os(SOCKET_ENV);
        let token = std::env::var_os(TOKEN_ENV);
        let (socket, token) = match (socket, token) {
            (Some(socket), Some(token)) => (socket, token),
            (None, None) => return Ok(None),
            _ => return Err(io::Error::other("incomplete process broker configuration")),
        };
        Self::connect(std::path::Path::new(&socket), &token.to_string_lossy()).map(Some)
    }

    fn connect(socket: &std::path::Path, token: &str) -> io::Result<Self> {
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::other("invalid process broker token"));
        }
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut hello = [0_u8; 65];
        hello[0] = b'B';
        hello[1..].copy_from_slice(token.as_bytes());
        stream.write_all(&hello)?;
        let mut ack = [0_u8; 1];
        stream.read_exact(&mut ack)?;
        if ack[0] != ACK {
            return Err(io::Error::other("process broker rejected registration"));
        }
        Ok(Self { stream })
    }

    pub(super) fn prepare(&self, command: &mut Command) {
        self.prepare_std(command.as_std_mut());
    }

    pub(super) fn prepare_std(&self, command: &mut std::process::Command) {
        command.env_remove(SOCKET_ENV).env_remove(TOKEN_ENV);
        let fd = self.stream.as_raw_fd();
        // The child cannot execute a command until the SDK has registered its
        // process group. Only async-signal-safe syscalls run after fork.
        unsafe {
            command.pre_exec(move || register_child(fd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[tokio::test]
    async fn child_registers_before_exec_and_uses_dedicated_group() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut hello = [0_u8; 65];
            stream.read_exact(&mut hello).unwrap();
            assert_eq!(hello[0], b'B');
            stream.write_all(&[ACK]).unwrap();
            let mut registered = [0_u8; 5];
            stream.read_exact(&mut registered).unwrap();
            assert_eq!(registered[0], b'P');
            let pid = u32::from_le_bytes(registered[1..].try_into().unwrap());
            assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
            stream.write_all(&[ACK]).unwrap();
            let mut settled = [0_u8; 1];
            stream.read_exact(&mut settled).unwrap();
            assert_eq!(settled[0], b'E');
            assert_eq!(
                unsafe { libc::kill(-(pid as i32), 0) },
                0,
                "anchor must reserve PGID until broker releases it"
            );
            stream.write_all(&[ACK]).unwrap();
            pid
        });
        let registration = Registration::connect(&socket, &"a".repeat(64)).unwrap();
        let mut command = Command::new("sh");
        command.arg("-c").arg("exit 0");
        command.process_group(0);
        registration.prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        assert_eq!(server.join().unwrap(), pid);
        assert!(child.wait().await.unwrap().success());
    }
}

fn register_child(fd: libc::c_int) -> io::Result<()> {
    let mut message = [0_u8; 5];
    message[0] = b'P';
    message[1..].copy_from_slice(&(unsafe { libc::getpid() } as u32).to_le_bytes());
    write_all_fd(fd, &message)?;
    let mut ack = [0_u8; 1];
    read_exact_fd(fd, &mut ack)?;
    if ack[0] != ACK {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    let command_pid = unsafe { libc::fork() };
    if command_pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if command_pid == 0 {
        return Ok(());
    }

    // This process stays the group leader and keeps its numeric PGID reserved
    // until the SDK confirms all command descendants have left the group.
    // The grandchild returns to std::process::Command's exec path. Close the
    // exec-error pipe in the anchor so Command::spawn need not wait for it.
    let fd = close_anchor_descriptors(fd);
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(command_pid, &mut status, 0) };
        if waited == command_pid {
            break;
        }
        if waited < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        unsafe {
            libc::_exit(127);
        }
    }
    if write_all_fd(fd, b"E").is_err() {
        unsafe {
            libc::_exit(127);
        }
    }
    let mut release = [0_u8; 1];
    if read_exact_fd(fd, &mut release).is_err() || release[0] != ACK {
        unsafe {
            libc::_exit(127);
        }
    }
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        127
    };
    unsafe {
        libc::_exit(code);
    }
}

fn close_anchor_descriptors(fd: libc::c_int) -> libc::c_int {
    // The broker stream is the only descriptor the anchor retains. Closing
    // stdio permits wait_with_output to see EOF after the command exits.
    let retained = if fd == 3 {
        3
    } else {
        unsafe { libc::dup2(fd, 3) }
    };
    if retained < 0 {
        unsafe {
            libc::_exit(127);
        }
    }
    for descriptor in 0..3 {
        unsafe {
            libc::close(descriptor);
        }
    }
    let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    let limit = if limit > 0 {
        limit.min(65_536) as libc::c_int
    } else {
        4096
    };
    for descriptor in 4..limit {
        unsafe {
            libc::close(descriptor);
        }
    }
    retained
}

fn write_all_fd(fd: libc::c_int, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
            continue;
        }
        if count == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}

fn read_exact_fd(fd: libc::c_int, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &mut bytes[count as usize..];
            continue;
        }
        if count == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}
