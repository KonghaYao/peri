use std::io::{self, Read, Write};

#[cfg(target_os = "windows")]
use portable_pty::SlavePty;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// PTY 会话封装。
///
/// 持有 master（用于 resize）、writer（用于 write）、child（用于 kill/wait）。
/// Windows 上额外持有 slave 避免 ConPTY 引用计数 bug（见 `_slave` 字段注释）。
/// reader 在 `spawn` 时返回给调用方，由调用方在 `spawn_blocking` 中读取。
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Windows 上必须保活到 session 结束。ConPTY 的 slave 是 pseudoconsole
    /// 对象句柄，提前 drop 会破坏引用计数，导致 `try_clone_reader` 拿到的
    /// read pipe 进入未连接状态，read 永久阻塞（wez/wezterm#4206、#1396）。
    /// Unix 上 slave 在 `spawn` 中立即 drop（见该函数注释）。
    ///
    /// 包装为 `Option` 以支持 `close_slave()` 显式关闭 pseudoconsole，
    /// 用于子进程退出后 unblock 读管道（Windows 上 ConPTY 在子进程退出后
    /// 不一定立即产生 EOF，需主动 drop slave）。
    #[cfg(target_os = "windows")]
    _slave: Option<Box<dyn SlavePty + Send>>,
}

/// 将输入中的行结束符统一归一化为 `\r\n`。
///
/// Windows 上 xterm.js 发来的 Enter 是 `\r`（0x0D），auto-injected 命令
/// 用 `\n`，但 PowerShell 的 PSReadLine 只认 `\r\n` 作为命令行终止符。
/// 因此将所有裸 `\r` 和裸 `\n` 都转为 `\r\n`（已有的 `\r\n` 保持不变）。
#[cfg(target_os = "windows")]
pub(super) fn normalize_crlf(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\r' => {
                // \r 开头 → 输出 \r\n，跳过后续 \n（如果存在）
                out.extend_from_slice(b"\r\n");
                if i + 1 < data.len() && data[i + 1] == b'\n' {
                    i += 1; // skip \n in \r\n
                }
            }
            b'\n' => {
                // 裸 \n → \r\n
                out.extend_from_slice(b"\r\n");
            }
            b => {
                out.push(b);
            }
        }
        i += 1;
    }
    out
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

        let reader = pair.master.try_clone_reader().map_err(io_err)?;
        let writer = pair.master.take_writer().map_err(io_err)?;

        // slave 生命周期平台差异：
        //
        // Unix：portable-pty 的 `spawn_command` 只关 `Child` 继承的 stdin/stdout/stderr
        // （见 portable-pty src/unix.rs:288），`UnixSlavePty` 自身仍持有 slave fd。
        // 必须显式 drop，否则 slave fd 持有导致 master read 永远不返回 EOF。
        //
        // Windows：slave 必须保活到 session 结束。提前 drop 会破坏 ConPTY 引用计数，
        // 导致 `try_clone_reader` 拿到的 read pipe 进入未连接状态（wez/wezterm#4206、#1396）。
        #[cfg(not(target_os = "windows"))]
        drop(pair.slave);

        Ok((
            Self {
                master: pair.master,
                writer,
                child,
                #[cfg(target_os = "windows")]
                _slave: Some(pair.slave),
            },
            reader,
        ))
    }

    /// 写 stdin 到 PTY。
    ///
    /// Windows 上将行结束符归一化为 `\r\n`（`normalize_crlf`），因为
    /// PowerShell 的 PSReadLine 只认 `\r\n` 作为命令行终止符。
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "windows")]
        {
            self.writer.write_all(&normalize_crlf(data))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.writer.write_all(data)
        }
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

    /// 关闭 pseudoconsole slave 句柄。
    ///
    /// 在子进程退出后调用，drop slave 句柄使 ConPTY 关闭，
    /// unblock 读管道让它返回 EOF。提前调用（子进程仍在运行时）可能
    /// 导致 read pipe 进入未连接状态，见 `_slave` 字段注释。
    ///
    /// Unix 上为 no-op（slave 已在 spawn 中 drop）。
    pub fn close_slave(&mut self) {
        #[cfg(target_os = "windows")]
        drop(self._slave.take());
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
