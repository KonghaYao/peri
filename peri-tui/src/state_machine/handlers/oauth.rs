//! OAuth authorisation handler.
//!
//! Wraps an [`peri_acp_types::event_data::OauthNeeded`] payload and dispatches
//! open/cancel keys (Enter/o = open URL in browser, Esc/q/c = dismiss).
//!
//! Phase 1.4: implements `render` to draw a bordered popup showing the server
//! name + auth URL (highlighted) + open-status feedback + action hints.
//! `desired_height` returns a fixed compact size (7 lines).

use peri_acp_types::event_data::OauthNeeded;
use peri_widgets::BorderedPanel;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use super::super::state::{Handler, HandlerOutput};
use crate::ui::theme;

/// Handler for an `"oauth-needed"` event. Holds the OAuth request and
/// tracks whether the user has opted to open the auth URL.
#[derive(Debug)]
pub struct OauthHandler {
    /// The OAuth authorisation request received from the ACP layer.
    pub request: OauthNeeded,
    /// Whether the user has already opened the auth URL in this session.
    url_opened: bool,
}

impl OauthHandler {
    /// Create a new handler from an oauth-needed payload.
    pub fn new(request: OauthNeeded) -> Self {
        Self {
            request,
            url_opened: false,
        }
    }
}

impl Handler for OauthHandler {
    fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        let title = format!("OAuth: {}", self.request.server_name);
        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::THINKING))
        .render(frame, area);

        let mut lines: Vec<Line> = Vec::new();

        // ── Prompt hint ───────────────────────────────────────────────
        lines.push(Line::from(Span::styled(
            "Authorise this MCP server by visiting the URL below:",
            Style::default().fg(theme::TEXT),
        )));

        // ── URL line — highlighted (SAGE on DarkGray) ────────────────
        lines.push(Line::from(Span::styled(
            self.request.auth_url.clone(),
            Style::default()
                .fg(theme::SAGE)
                .bg(ratatui::style::Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));

        // ── Status feedback (post-open confirmation) ─────────────────
        lines.push(Line::from(""));
        if self.url_opened {
            lines.push(Line::from(Span::styled(
                "✓ Opened in your browser. Paste the callback code in the main input.",
                Style::default().fg(theme::SAGE),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Press Enter (or o) to open the URL in your default browser.",
                Style::default().fg(theme::MUTED),
            )));
        }

        // ── Hint line ─────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                " Enter/o: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("open URL", Style::default().fg(theme::SAGE)),
            Span::styled(
                "   Esc/q/c: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("cancel", Style::default().fg(theme::ERROR)),
        ]));

        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        // Fixed compact layout:
        //   prompt (1) + URL (1) + spacer (1) + status (1) + spacer (1)
        //   + hint (1) + border (2) = 8 lines.
        8
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        match key.code {
            KeyCode::Enter => {
                self.url_opened = true;
                HandlerOutput::Submit(self.request.auth_url.clone())
            }
            KeyCode::Char('o' | 'O') => {
                self.url_opened = true;
                HandlerOutput::Submit(self.request.auth_url.clone())
            }
            KeyCode::Esc => HandlerOutput::Dismiss,
            KeyCode::Char('q' | 'Q' | 'c' | 'C') => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_esc};
    use super::*;

    fn make_request() -> OauthNeeded {
        OauthNeeded {
            server_name: "github-mcp".into(),
            auth_url: "https://github.com/login/oauth".into(),
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = OauthHandler::new(make_request());
        assert_eq!(h.request.server_name, "github-mcp");
        assert_eq!(h.request.auth_url, "https://github.com/login/oauth");
    }

    #[test]
    fn test_handler_initial_state() {
        let h = OauthHandler::new(make_request());
        assert!(!h.url_opened);
    }

    #[test]
    fn test_handle_key_enter_opens_url() {
        let mut h = OauthHandler::new(make_request());
        let output = h.handle_key(key_enter());
        assert!(matches!(output, HandlerOutput::Submit(ref s) if s.contains("github.com")));
        assert!(h.url_opened);
    }

    #[test]
    fn test_handle_key_o_opens_url() {
        let mut h = OauthHandler::new(make_request());
        assert!(matches!(h.handle_key(key('o')), HandlerOutput::Submit(_)));
        assert!(h.url_opened);
    }

    #[test]
    fn test_handle_key_uppercase_o_opens_url() {
        let mut h = OauthHandler::new(make_request());
        assert!(matches!(h.handle_key(key('O')), HandlerOutput::Submit(_)));
        assert!(h.url_opened);
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key_esc()), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_q_dismisses() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key('q')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_c_dismisses() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key('c')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_unknown_is_nothing() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key('x')), HandlerOutput::Nothing);
    }

    // ── Phase 1.4: render / desired_height ────────────────────────────

    #[test]
    fn test_desired_height_fixed_compact() {
        // OAuth popup 是固定大小（7 行内容 + 2 行边框 = 9，实际期望 8 行紧凑布局）。
        let h = OauthHandler::new(make_request());
        let height = h.desired_height(50, 100);
        assert!(
            (7..=10).contains(&height),
            "oauth popup height should be compact (7-10), got {height}"
        );
    }

    #[test]
    fn test_desired_height_same_before_after_open() {
        // 打开 URL 后高度应保持不变（紧凑固定布局）。
        let mut h = OauthHandler::new(make_request());
        let before = h.desired_height(50, 100);
        h.handle_key(key_enter());
        let after = h.desired_height(50, 100);
        assert_eq!(
            before, after,
            "desired_height should not change after url_opened"
        );
    }

    #[test]
    fn test_render_does_not_panic() {
        // render 应在 TestBackend 上成功绘制（验证 BorderedPanel + Paragraph 组合）。
        let h = OauthHandler::new(make_request());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("render should succeed");
    }

    #[test]
    fn test_render_after_open_does_not_panic() {
        // url_opened=true 时 render 也应安全（显示 "✓ Opened" 状态行）。
        let mut h = OauthHandler::new(make_request());
        h.handle_key(key_enter());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("render after open should succeed");
    }

    #[test]
    fn test_render_with_long_url_does_not_panic() {
        // 超长 URL 不应导致 panic（ratatui Paragraph 自动换行处理）。
        let req = OauthNeeded {
            server_name: "very-long-server-name-mcp".into(),
            auth_url: "https://example.com/oauth/authorize?client_id=12345&redirect_uri=http://localhost:3000/callback&scope=read+write+admin&state=abc123def456".into(),
        };
        let h = OauthHandler::new(req);
        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 40, 12));
            })
            .expect("render with long URL should succeed");
    }
}
