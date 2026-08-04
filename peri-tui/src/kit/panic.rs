//! Panic hook（TUI 专用）
//!
//! [Fix P0] 从 main.rs 移入 lib 侧，使 kit 组件能在 `ratatui::init()` **之后**
//! 重新安装 hook。`ratatui::init()` 会包装 panic hook（先 `restore()` 终端——
//! `disable_raw_mode` + `LeaveAlternateScreen` escape 序列写入 stdout——再调用
//! 原 hook）。若 agent 的 tokio task panic，包装 hook 会从**非渲染线程**向
//! stdout 写 escape 序列，与渲染循环并发写 → 终端退出 alt screen、raw mode
//! 被关 → 界面乱码崩溃。重装后 panic 只记录日志 + 通知 TUI，不破坏终端。

use std::sync::OnceLock;

/// 全局 panic 通知通道 sender（OnceLock 保证只初始化一次）
static PANIC_NOTIFY: OnceLock<tokio::sync::mpsc::UnboundedSender<String>> = OnceLock::new();

/// 格式化 panic 信息为可读字符串（消息 + 位置 + backtrace）
fn format_panic_message(panic_info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    };

    let location = panic_info
        .location()
        .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
        .unwrap_or_else(|| "unknown location".to_string());

    // 自动捕获 backtrace（无需手动设置 RUST_BACKTRACE=1）
    let backtrace = std::backtrace::Backtrace::capture();
    let bt_str = match backtrace.status() {
        std::backtrace::BacktraceStatus::Captured => format!("\n{}", backtrace),
        _ => String::new(),
    };

    format!("'{}'\n  at {}{}", payload, location, bt_str)
}

/// 安装自定义 panic hook：
/// - 通过 tracing::error! 记录到日志文件（不写 stderr）
/// - 通过 PANIC_NOTIFY 通道通知 TUI
///
/// 可重复调用（后一次覆盖前一次）。AppShell mount（`ratatui::init()` 之后）
/// 会再次调用，覆盖 ratatui 包装的 hook，避免任意线程 panic 时向 stdout 写
/// escape 序列导致终端乱码。
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format_panic_message(panic_info);
        tracing::error!("thread panicked at {}", msg);
        if let Some(tx) = PANIC_NOTIFY.get() {
            let _ = tx.send(msg);
        }
    }));
}

/// 创建 panic 通知通道并安装自定义 panic hook。
/// 必须在 enable_raw_mode() 之前调用。
/// 返回 UnboundedReceiver 供 TUI 消费。
pub fn init_panic_notify() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = PANIC_NOTIFY.set(tx);
    install_panic_hook();
    rx
}
