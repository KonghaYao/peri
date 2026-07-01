//! ratatui-kit OAuthPopup component.
//!
//! OAuth 授权弹窗：显示授权服务器信息和 URL，提供输入授权码、打开浏览器等操作。
//!
//! Phase 7：完整 UI + 键盘交互（输入授权码）。Phase 8 接入 Handler 回调。

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

use crate::ui::theme;

/// 模拟授权信息
struct OAuthInfo {
    server_name: &'static str,
    auth_url: &'static str,
    hint: &'static str,
}

fn mock_oauth_info() -> OAuthInfo {
    OAuthInfo {
        server_name: "github.com",
        auth_url: "https://github.com/login/oauth/authorize?client_id=abc123&scope=repo",
        hint: "Visit the URL below to authorize, then paste the code here.",
    }
}

#[component]
pub fn OAuthPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let oauth = mock_oauth_info();
    let code = hooks.use_state(String::new);

    hooks.use_local_events({
        let code = code;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match (key.modifiers, key.code) {
                    // Phase 8: Ctrl+O → on_open_browser
                    (KeyModifiers::CONTROL, KeyCode::Char('o') | KeyCode::Char('O')) => {
                        // Phase 8: on_open_browser.call(())
                    }
                    // Phase 8: Enter → on_submit(code)
                    (KeyModifiers::NONE, KeyCode::Backspace) => {
                        code.write().pop();
                    }
                    (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                        code.write().push(c);
                    }
                    _ => {}
                }
            }
        }
    });

    let current_code = code.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(""));
    // 服务器名
    lines.push(
        Line::from(format!("  Server: {}", oauth.server_name))
            .fg(theme::SAGE)
            .bold(),
    );
    lines.push(Line::from(""));
    // 提示文字
    lines.push(Line::from(format!("  {}", oauth.hint)).fg(theme::TEXT));
    lines.push(Line::from(""));
    // URL（截断显示）
    let truncated_url = if oauth.auth_url.len() > 44 {
        format!("{}...", &oauth.auth_url[..44])
    } else {
        oauth.auth_url.to_string()
    };
    lines.push(Line::from(format!("  {}", truncated_url)).fg(theme::MUTED));
    lines.push(Line::from(""));
    // 输入区域
    if current_code.is_empty() {
        lines.push(Line::from("  [paste authorization code here]").fg(theme::DIM));
    } else {
        let masked: String = current_code.chars().map(|_| '*').collect();
        lines.push(Line::from(format!("  [ {} ]", masked)).fg(theme::SAGE));
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("  Ctrl+O: open browser  |  Enter: submit  |  Esc: cancel").fg(theme::DIM),
    );

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" OAuth Authorization ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(10),
        ) {
            Text(text: text_render)
        }
    )
}
