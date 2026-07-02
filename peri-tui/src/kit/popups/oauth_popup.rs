//! ratatui-kit OAuthPopup component.
//!
//! OAuth 授权弹窗：从 `OAUTH_INFO` atom 读取真实授权信息（由 ACP server
//! `OauthNeeded` 事件写入）。用户可：
//! - **Ctrl+O**：调用系统 `open` 命令打开浏览器到 `auth_url`（best-effort，
//!   失败时记日志，不 panic）
//! - **Enter**：关闭 popup（ACP server 自身的 OAuth 完成回调会再推送状态事件
//!   刷新 UI；本地不缓存授权码，避免误用陈旧凭据）
//! - **Esc**：取消（由全局 Esc 链处理）
//!
//! I20-D：替换原 mock_oauth_info() 写死数据——现在 popup 展示的是 agent 实际
//! 触发 OAuth 的 server_name + auth_url，用户能据此判断该不该授权。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

use crate::kit::atoms::OAUTH_INFO;
use crate::kit::popup_overlay::close_popup;
use crate::kit::theme;

#[component]
pub fn OAuthPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // I20-D：从 OAUTH_INFO atom 读取真实数据。atom 由 dispatch_and_notify 在
    // OauthNeeded 事件时写入。None 时显示占位（理论上不会发生——popup 只有在
    // POPUP_KIND=OAuth 时渲染，而该状态只在写入 OAUTH_INFO 同步设置）。
    let info_store = hooks.use_store(*OAUTH_INFO.get().unwrap());
    let info = info_store.read().clone();
    let _ = info_store;

    hooks.use_local_events({
        let info_for_open = info.clone();
        move |event: Event| {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                match (key.modifiers, key.code) {
                    // Ctrl+O：调用系统 open 命令打开 auth_url
                    // 仅 macOS 使用 `open`；Linux/Windows 平台 fallback 记日志
                    (KeyModifiers::CONTROL, KeyCode::Char('o') | KeyCode::Char('O')) => {
                        if let Some(info) = &info_for_open {
                            open_auth_url_in_browser(&info.auth_url);
                        }
                    }
                    // Enter：关闭 popup——OAuth 完成由 ACP server 推送事件刷新
                    // I21-C：close_popup 内部统一清空 OAUTH_INFO atom
                    (KeyModifiers::NONE, KeyCode::Enter) => {
                        close_popup();
                    }
                    // 退格：no-op（不再支持本地输入授权码——避免与 ACP 完成回调冲突）
                    (KeyModifiers::NONE, KeyCode::Backspace) => {}
                    (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(_)) => {}
                    _ => {}
                }
            }
        }
    });

    let mut lines: Vec<Line<'_>> = Vec::new();

    match &info {
        None => {
            // 理论上不会渲染此分支——POPUP_KIND=OAuth 暗示 OAUTH_INFO 已写入
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No OAuth request pending.")
                    .fg(theme::MUTED)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(theme::DIM));
        }
        Some(oauth) => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!("  Server: {}", oauth.server_name))
                    .fg(theme::SAGE)
                    .bold(),
            );
            lines.push(Line::from(""));
            lines.push(
                Line::from("  MCP server requires authorization. Visit the URL below,")
                    .fg(theme::TEXT),
            );
            lines.push(
                Line::from("  complete the flow, then press Enter to close this dialog.")
                    .fg(theme::TEXT),
            );
            lines.push(Line::from(""));
            // URL 截断用 chars().take(N) 避免 CJK 字节切片 panic（I19-C 同模式）
            let url_chars: Vec<char> = oauth.auth_url.chars().collect();
            let truncated_url = if url_chars.len() > 44 {
                let prefix: String = url_chars.into_iter().take(44).collect();
                format!("{}...", prefix)
            } else {
                oauth.auth_url.clone()
            };
            lines.push(Line::from(format!("  {}", truncated_url)).fg(theme::MUTED));
            lines.push(Line::from(""));
            lines.push(
                Line::from("  Ctrl+O: open in browser  |  Enter: close  |  Esc: cancel")
                    .fg(theme::DIM),
            );
        }
    }

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" OAuth Authorization ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(11),
        ) {
            Text(text: text_render)
        }
    )
}

/// I20-D：用系统命令打开 auth_url——best-effort，失败仅记日志不报错。
///
/// macOS 用 `open`，Linux 用 `xdg-open`，Windows 用 `cmd /C start`。任何错误
/// （命令不存在、权限不足等）都不应 panic——OAuth 弹窗仍展示完整 URL，
/// 用户可手动复制到浏览器。
fn open_auth_url_in_browser(url: &str) {
    use std::process::Command;

    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };

    match Command::new(program).args(&args).spawn() {
        Ok(_child) => {
            tracing::info!(program, url, "OAuthPopup: 已调用系统命令打开浏览器");
        }
        Err(e) => {
            tracing::warn!(
                program,
                error = %e,
                "OAuthPopup: 调用系统浏览器失败，用户需手动复制 URL"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_auth_url_does_not_panic_on_invalid_platform() {
        // 即使 xdg-open 不存在（CI 容器），此调用也应安全返回而非 panic
        open_auth_url_in_browser("https://example.com/oauth");
    }
}
