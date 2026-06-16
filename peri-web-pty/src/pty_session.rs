use std::io::{self, Read, Write};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// PTY 会话封装。
///
/// 持有 master（用于 resize）、writer（用于 write）、child（用于 kill/wait）。
/// reader 在 `spawn` 时返回给调用方，由调用方在 `spawn_blocking` 中读取。
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    /// Spawn 一个 shell 进程到 PTY，返回 (PtySession, reader)。
    ///
    /// reader 是阻塞 `Read`，调用方应在 `spawn_blocking` 中循环读取。
    pub fn spawn(
        shell: &str,
        args: &[&str],
        cols: u16,
        rows: u16,
        cwd: Option<&str>,
    ) -> io::Result<(Self, Box<dyn Read + Send>)> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }

        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        // 释放 slave：portable-pty 要求 slave drop 后 master 才能在子进程退出时 EOF
        drop(pair.slave);

        // reader 必须在 spawn 之后 clone：按 portable-pty 官方示例顺序。
        // Windows ConPTY 上若在 spawn 之前 DuplicateHandle，clone 出来的 pipe
        // handle 处于"未连接"状态，后续 read 会永久阻塞读不到任何字节。
        // Unix 用 dup 复制 fd 共享同一 PTY 流，顺序不敏感。
        let reader = pair.master.try_clone_reader().map_err(io_err)?;

        let writer = pair.master.take_writer().map_err(io_err)?;

        Ok((
            Self {
                master: pair.master,
                writer,
                child,
            },
            reader,
        ))
    }

    /// 写 stdin 到 PTY。
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data)
    }

    /// 调整 PTY 尺寸。
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)
    }

    /// 非阻塞查询子进程退出码。返回 `Ok(None)` 表示尚未退出。
    pub fn try_wait_exit(&mut self) -> io::Result<Option<i32>> {
        let status = self.child.try_wait().map_err(io_err)?;
        // portable-pty 的 ExitStatus::exit_code() 返回 u32（始终有值），
        // 与 std::process::ExitStatus::code()（Option<i32>）不同。
        // try_wait 返回 Option<ExitStatus>：None=未退出，Some=已退出。
        Ok(status.map(|s| s.exit_code() as i32))
    }

    /// Kill 子进程。已退出时返回 Ok(())。
    pub fn kill(&mut self) -> io::Result<()> {
        match self.child.kill() {
            Ok(()) => Ok(()),
            // 已经退出的进程 kill 失败是正常的
            Err(e) if e.kind() == io::ErrorKind::Other => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // 尽力 kill，portable-pty 在 master drop 时会清理
        let _ = self.child.kill();
    }
}

/// 把 anyhow 风格错误转成 io::Error。
fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}
