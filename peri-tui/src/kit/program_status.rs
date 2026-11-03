//! OSC 7501 程序状态上报（Program Status Protocol）。
//!
//! 终端与 agent inbox（herdr、cmux、Ghostty 等）靠这条 OSC 判断 peri 是在运行、
//! 等待用户还是已完成，无需抓屏。规范：
//! <https://www.superlogical.com/rex/docs/build/program-status>。
//!
//! 只在 stdout 是 TTY 时发送；规范要求终端忽略未知 OSC，因此不先做 feature
//! detection。`PERI_PROGRAM_STATUS=0|off|false|no` 可关闭上报。

use std::io::{IsTerminal, Write};

use crate::kit::atoms::PROGRAM_STATUS;

/// `app` 的稳定机器名——外层按它分组与展示。
const APP: &str = "peri";

/// 环境读取面（测试注入用）。
type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// 上报给终端的程序状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramStatus {
    /// 空闲：等待用户下一条指令。
    Idle,
    /// 正在运行。
    Working,
    /// 无法继续，等待用户处理 [`BlockedKind`]。
    Blocked(BlockedKind),
    /// 本轮完成，结果未被用户查看。
    Done,
    /// 失败并停止。
    Error,
}

/// `blocked` 的具体原因（规范的 `kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    /// 需要批准某个操作。
    Permission,
    /// 需要用户回答问题。
    Question,
    /// 需要登录或凭据。
    Auth,
}

impl ProgramStatus {
    fn state(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked(_) => "blocked",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

/// 一条报告：`ESC ] 7501 ; key=value[:...] ESC \`。
///
/// 规范允许 BEL 终止；用 ST 与 herdr、Claude Code 的实际发送保持一致。
fn sequence(status: ProgramStatus) -> String {
    let mut pairs = format!("state={}", status.state());
    if let ProgramStatus::Blocked(kind) = status {
        let kind = match kind {
            BlockedKind::Permission => "permission",
            BlockedKind::Question => "question",
            BlockedKind::Auth => "auth",
        };
        pairs.push_str(":kind=");
        pairs.push_str(kind);
    }
    pairs.push_str(":app=");
    pairs.push_str(APP);
    format!("\x1b]7501;{pairs}\x1b\\")
}

/// `state=clear` 删除根记录及其子记录。
fn clear_sequence() -> &'static str {
    "\x1b]7501;state=clear\x1b\\"
}

/// 由 bridge 状态更新待上报值；原子只在变化时写入，避免多余重渲染。
pub fn publish(status: ProgramStatus) {
    {
        let current = *PROGRAM_STATUS.state().read();
        if current == status {
            return;
        }
    }
    *PROGRAM_STATUS.state().write() = status;
}

/// 发送一条状态报告。写失败只记 debug——终端已断开时没有可恢复动作。
pub fn report(status: ProgramStatus) {
    let is_tty = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = emit(status, &mut stdout, &process_env, is_tty) {
        tracing::debug!(%error, "program status: OSC 7501 write failed");
    }
}

/// 正常退出前清除记录——`done`/`error` 会跨进程存活，不清会一直留在外层。
pub fn clear() {
    let is_tty = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = emit_clear(&mut stdout, &process_env, is_tty) {
        tracing::debug!(%error, "program status: OSC 7501 clear failed");
    }
}

fn process_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

fn emit(
    status: ProgramStatus,
    writer: &mut impl Write,
    env: Env<'_>,
    is_tty: bool,
) -> std::io::Result<()> {
    if !is_tty || !enabled(env) {
        return Ok(());
    }
    write_sequence(writer, &sequence(status))
}

fn emit_clear(writer: &mut impl Write, env: Env<'_>, is_tty: bool) -> std::io::Result<()> {
    if !is_tty || !enabled(env) {
        return Ok(());
    }
    write_sequence(writer, clear_sequence())
}

fn write_sequence(writer: &mut impl Write, sequence: &str) -> std::io::Result<()> {
    writer.write_all(sequence.as_bytes())?;
    writer.flush()
}

/// `PERI_PROGRAM_STATUS` 显式关闭；未知取值按自动（开启）处理，与 `PERI_CLIPBOARD` 一致。
fn enabled(env: Env<'_>) -> bool {
    !matches!(
        env("PERI_PROGRAM_STATUS")
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

#[cfg(test)]
#[path = "program_status_test.rs"]
mod tests;
