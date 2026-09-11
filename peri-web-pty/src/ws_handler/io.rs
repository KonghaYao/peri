//! Unix PTY readiness：只有连接 owner 的实例转换为非阻塞 I/O。
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use tokio::io::unix::AsyncFd;

pub(super) struct PtyIo(AsyncFd<File>);

impl PtyIo {
    /// The session keeps the borrowed master alive throughout this conversion.
    pub(super) fn new(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let owned = fd.try_clone_to_owned()?;
        let raw = owned.as_raw_fd();
        // SAFETY: raw is owned by this function; flags preserve the current descriptor mode.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this descriptor and its session clones are exclusively owned by the connection.
        if unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(AsyncFd::new(File::from(owned))?))
    }

    pub(super) fn try_read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.0.get_ref().read(buffer) {
            // Linux PTYs report EIO after the final slave closes; portable-pty treats it as EOF too.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            result => result,
        }
    }

    pub(super) async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|_| self.try_read(buffer)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }

    pub(super) async fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.writable().await?;
            match ready.try_io(|fd| fd.get_ref().write(bytes)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }
}
