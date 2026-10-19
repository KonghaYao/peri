//! 文本复制：通道选择 + 写入。
//!
//! 本机有显示环境时写系统剪贴板（arboard）；SSH、herdr 与无显示环境下 arboard
//! 够不到用户面前的剪贴板，改经终端 OSC 52 序列把内容交回**客户端终端**。
//!
//! 选择规则见 [`select_channel`]，`PERI_CLIPBOARD` 可显式覆盖。主通道失败时用
//! 另一条通道兜底（[`copy_with`]）——环境探测不全（容器、`sudo` 丢环境变量）与
//! 终端不支持 OSC 52 时互为补偿。
//!
//! 终端通道在 tmux 下同时发送两种形式：tmux 原生 OSC 52（要求 `set-clipboard on`
//! 且客户端终端 terminfo 带 `Ms`）与 `\x1bPtmux;…\x1b\\` DCS 透传（要求
//! `allow-passthrough on`）。这两项配置默认分别为 `external` 与 `off`，互不覆盖，
//! 故两条路都发；未生效的一路由 tmux 丢弃，不会落在屏幕上。

use std::io::Write;

use anyhow::Context;
use base64::Engine;

/// 剪贴板写入通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// 本机系统剪贴板（arboard）。
    Native,
    /// 终端 OSC 52——由客户端终端写入用户本地剪贴板。
    Terminal,
}

impl Channel {
    fn other(self) -> Self {
        match self {
            Self::Native => Self::Terminal,
            Self::Terminal => Self::Native,
        }
    }
}

/// 终端复用器——决定 OSC 52 的封装形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Multiplexer {
    /// 无复用器（或无需特殊封装）：只发原生 OSC 52。
    None,
    /// tmux：原生 OSC 52 + DCS 透传副本。
    Tmux,
}

/// 环境读取面（测试注入用）。
type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn process_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

fn select_channel(env: Env<'_>) -> Channel {
    if let Some(channel) = override_channel(env) {
        return channel;
    }
    if is_client_clipboard(env) || !native_clipboard_reachable(env) {
        Channel::Terminal
    } else {
        Channel::Native
    }
}

/// `PERI_CLIPBOARD` 显式覆盖：`native` 强制系统剪贴板、`osc52`/`terminal` 强制终端
/// 剪贴板；其余取值（含 `auto`）与未设置等价，交给自动探测。
fn override_channel(env: Env<'_>) -> Option<Channel> {
    match env("PERI_CLIPBOARD")?.trim().to_ascii_lowercase().as_str() {
        "native" => Some(Channel::Native),
        "osc52" | "terminal" => Some(Channel::Terminal),
        _ => None,
    }
}

/// 剪贴板落在**用户面前那台机器**：SSH 会话或 herdr 转发，arboard 写的是服务端
/// 剪贴板，写不到。
fn is_client_clipboard(env: Env<'_>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "HERDR_ENV"]
        .into_iter()
        .any(|name| env(name).is_some())
}

/// arboard 在非 macOS 的 Unix 上依赖 X11/Wayland：两者都没设置时初始化必然失败
/// （arboard 3.6.1 `platform/linux/mod.rs`：无 `WAYLAND_DISPLAY` 直接走 X11）。
///
/// [Why 不复用 `is_client_clipboard`] tmux/screen 可能在本机运行——本机有显示环境时
/// 系统剪贴板就是用户面前的剪贴板，比多绕一层 OSC 52 更可靠。
fn native_clipboard_reachable(env: Env<'_>) -> bool {
    !native_clipboard_needs_display()
        || env("DISPLAY").is_some()
        || env("WAYLAND_DISPLAY").is_some()
}

const fn native_clipboard_needs_display() -> bool {
    cfg!(all(unix, not(target_os = "macos")))
}

fn multiplexer(env: Env<'_>) -> Multiplexer {
    if env("TMUX").is_some() {
        Multiplexer::Tmux
    } else {
        Multiplexer::None
    }
}

pub(crate) fn copy_text(text: &str) -> anyhow::Result<()> {
    let env: Env<'_> = &process_env;
    copy_with(
        text,
        select_channel(env),
        multiplexer(env),
        &mut std::io::stdout().lock(),
        native_set_text,
    )
}

/// 系统剪贴板是否即用户面前的剪贴板——与复制同一套探测（[`select_channel`]）。
///
/// 粘贴侧据此判断"本机读不到内容"要不要向用户解释：远端（SSH/herdr）与无显示
/// 环境下 arboard 读到的是另一台机器的剪贴板，或根本读不到。
pub(crate) fn system_clipboard_is_authority() -> bool {
    select_channel(&process_env) == Channel::Native
}

/// 按 `channel` 写入；失败时用另一条通道重试，两条都失败才返回错误。
fn copy_with(
    text: &str,
    channel: Channel,
    mux: Multiplexer,
    writer: &mut impl Write,
    native: impl Fn(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let primary_error = match write_via(channel, text, mux, writer, &native) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let fallback = channel.other();
    match write_via(fallback, text, mux, writer, &native) {
        Ok(()) => {
            tracing::warn!(
                channel = ?channel,
                fallback = ?fallback,
                error = ?primary_error,
                "剪贴板主通道写入失败，已回落备用通道"
            );
            Ok(())
        }
        // 主通道错误为 cause：日志里先看到失败原因，再看到兜底也失败。
        Err(fallback_error) => {
            Err(primary_error.context(format!("fallback {fallback:?} failed: {fallback_error:#}")))
        }
    }
}

fn write_via(
    channel: Channel,
    text: &str,
    mux: Multiplexer,
    writer: &mut impl Write,
    native: &impl Fn(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match channel {
        Channel::Native => native(text),
        Channel::Terminal => {
            write_terminal(writer, text, mux).context("Failed to send terminal clipboard write")
        }
    }
}

/// 进程级常驻的原生剪贴板句柄。
///
/// [Why 不每次创建] arboard 3.6.1 的 X11 实现在**最后一个** `Clipboard` 实例
/// 释放时让 clipboard manager 接管，然后销毁提供数据的窗口并结束服务线程；
/// 没有 manager 或接管失败时，`set_text` 虽已返回 Ok，随后却无法粘贴（F9）。
/// 进程级句柄让 provider 的生命周期覆盖 TUI 使用期。
fn native_clipboard() -> &'static std::sync::Mutex<Option<arboard::Clipboard>> {
    static CLIPBOARD: std::sync::OnceLock<std::sync::Mutex<Option<arboard::Clipboard>>> =
        std::sync::OnceLock::new();
    CLIPBOARD.get_or_init(|| std::sync::Mutex::new(None))
}

fn native_set_text(text: &str) -> anyhow::Result<()> {
    let mut guard = native_clipboard()
        .lock()
        .map_err(|_| anyhow::anyhow!("native clipboard lock poisoned"))?;
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().context("Failed to initialize native clipboard")?);
    }
    guard
        .as_mut()
        .expect("initialized above")
        .set_text(text)
        .context("Failed to write native clipboard")
}

/// tmux 下先发原生 OSC 52，再发 DCS 透传副本——两条投递路径依赖不同 tmux 配置。
fn write_terminal(writer: &mut impl Write, text: &str, mux: Multiplexer) -> std::io::Result<()> {
    write_osc52(writer, text)?;
    if mux == Multiplexer::Tmux {
        writer.write_all(tmux_passthrough(text).as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

/// OSC 52：`ESC ] 52 ; c ; <base64> BEL`（`c` = 剪贴板，非 primary selection）。
fn write_osc52(writer: &mut impl Write, text: &str) -> std::io::Result<()> {
    writer.write_all(osc52(text).as_bytes())?;
    writer.flush()
}

fn osc52(text: &str) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{payload}\x07")
}

/// tmux DCS 透传：`ESC P tmux ; <OSC 52> ESC \`。
///
/// [Why 内层 ESC 必须双写] tmux 的 passthrough 协议把 `ESC` 当作自身的转义
/// 字符：客户端内容里的每个 `ESC` 都要写成 `ESC ESC`，tmux 转发时再还原为
/// 单个 `ESC`（tmux FAQ / input.c 的 DCS 状态表）。不双写会丢掉 OSC 52
/// 的引导 ESC，外层终端只看到 `]52;…` 文本或直接丢弃（F8）。
fn tmux_passthrough(text: &str) -> String {
    let payload = osc52(text).replace('\x1b', "\x1b\x1b");
    format!("\x1bPtmux;{payload}\x1b\\")
}

#[cfg(test)]
#[path = "clipboard_test.rs"]
mod tests;
