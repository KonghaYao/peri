//! 平台 opener：只允许 http/https，参数直接传给平台命令（不经过 shell）。
//!
//! 外部 MCP URL elicitation 的目标由 server 控制：绝不能把 URL 拼进 shell
//! 字符串（`&` / `%` 可注入），也绝不自动预取。调用方必须先让用户确认域名。

use std::process::Command;

/// 仅接受 `http://` / `https://`；其余 scheme 一律拒绝。
pub fn validate_url(url: &str) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(())
    } else {
        Err("only http/https URLs can be opened".to_string())
    }
}

/// 用平台 opener 打开用户已确认的 URL。
///
/// 失败时调用方保留可复制链接（面板已展示去掉 query 的 URL）。
pub fn open(url: &str) -> Result<(), String> {
    validate_url(url)?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Err("no graphical session is available".to_string());
        }
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // `cmd /C start` 会把 URL 中的 `&`/`%` 当成 shell 语法；rundll32 用
        // argv 直传，不经过命令解释器。
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        return Err("opening URLs is not supported on this platform".to_string());
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to launch the platform opener: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_are_openable() {
        assert!(validate_url("https://example.com/connect").is_ok());
        assert!(validate_url("http://localhost:3000").is_ok());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("example.com").is_err());
    }

    #[test]
    fn invalid_scheme_is_rejected_before_any_spawn() {
        assert_eq!(
            open("file:///tmp/x").unwrap_err(),
            "only http/https URLs can be opened"
        );
    }
}
