//! Welcome Card — 空消息时显示品牌 Logo + 功能提示

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, ui::theme};

/// ASCII Art Logo（"PERI"，ansi_shadow 字体，6 行）
const LOGO: &[&str] = &[
    "██████╗ ███████╗██████╗ ██╗",
    "██╔══██╗██╔════╝██╔══██╗██║",
    "██████╔╝█████╗  ██████╔╝██║",
    "██╔═══╝ ██╔══╝  ██╔══██╗██║",
    "██║     ███████╗██║  ██║██║",
    "╚═╝     ╚══════╝╚═╝  ╚═╝╚═╝",
];

/// 窄屏阈值：低于此宽度跳过 ASCII Art Logo
const NARROW_THRESHOLD: u16 = 50;

/// 渲染 Welcome Card（空消息时替代聊天区内容）
pub(crate) fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let lc = &app.services.lc;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let narrow = area.width < NARROW_THRESHOLD;

    // ── Logo 区域 ────────────────────────────────────────────────────────
    if narrow {
        // 窄屏：单行文字标题
        lines.push(Line::from(Span::styled(
            "Peri",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        // 宽屏：ASCII Art Logo
        lines.push(Line::from(""));
        for row in LOGO {
            lines.push(Line::from(Span::styled(
                row.to_string(),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    // ── 副标题 ──────────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        lc.tr("welcome-title"),
        Style::default().fg(theme::MUTED),
    )));

    // ── 分隔线 ──────────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        lc.tr("welcome-divider"),
        Style::default().fg(theme::DIM),
    )));

    // ── 功能亮点 ────────────────────────────────────────────────────────
    lines.push(Line::from(""));

    let features = [
        lc.tr("welcome-feature-code"),
        lc.tr("welcome-feature-files"),
        lc.tr("welcome-feature-agents"),
    ];

    for feat in &features {
        lines.push(Line::from(vec![
            Span::styled(" • ", Style::default().fg(theme::ACCENT)),
            Span::styled(feat.clone(), Style::default().fg(theme::TEXT)),
        ]));
    }

    // ── 命令提示 ────────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" /model", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::MUTED)),
        Span::styled("/history", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::MUTED)),
        Span::styled("/help", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::MUTED)),
        Span::styled("/agents", Style::default().fg(theme::WARNING)),
    ]));

    // ── 首次使用引导（未配置 Provider 时显示）───────────────────────────
    let has_provider = !app.services.peri_config.read().config.providers.is_empty();
    if !has_provider {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                " ▶ ",
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                lc.tr("welcome-login-hint-1"),
                Style::default().fg(theme::TEXT),
            ),
            Span::styled(
                "/login",
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                lc.tr("welcome-login-hint-2"),
                Style::default().fg(theme::TEXT),
            ),
        ]));
    }

    // ── 快捷键提示 ──────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Esc", Style::default().fg(theme::DIM)),
        Span::styled(
            lc.tr("welcome-shortcut-quit"),
            Style::default().fg(theme::DIM),
        ),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("Ctrl+C", Style::default().fg(theme::DIM)),
        Span::styled(
            lc.tr("welcome-shortcut-stop"),
            Style::default().fg(theme::DIM),
        ),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("Shift+Enter", Style::default().fg(theme::DIM)),
        Span::styled(
            lc.tr("welcome-shortcut-newline"),
            Style::default().fg(theme::DIM),
        ),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("Shift+Tab", Style::default().fg(theme::DIM)),
        Span::styled(
            lc.tr("welcome-shortcut-mode"),
            Style::default().fg(theme::DIM),
        ),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled(
            crate::event::keyboard::cycle_model_label(),
            Style::default().fg(theme::DIM),
        ),
        Span::styled(
            lc.tr("welcome-shortcut-model"),
            Style::default().fg(theme::DIM),
        ),
    ]));

    // ── 动态信息 ────────────────────────────────────────────────────────
    // 当前模型/Provider 信息
    let provider = &app.services.provider_name;
    let model = &app.services.model_name;
    if !provider.is_empty() || !model.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" ⚡ ", Style::default().fg(theme::ACCENT)),
            Span::styled(
                format!("{} / {}", provider, model),
                Style::default().fg(theme::TEXT),
            ),
        ]));
    }

    let skills_count = app.session_mgr.current().commands.skills.len();
    if skills_count > 0 {
        lines.push(Line::from(vec![
            Span::styled(" #", Style::default().fg(theme::WARNING)),
            Span::styled(
                lc.tr_args(
                    "welcome-skills-available",
                    &[("count".into(), (skills_count as i64).into())],
                ),
                Style::default().fg(theme::TEXT),
            ),
        ]));
    }

    // ── 居中渲染 ────────────────────────────────────────────────────────
    let content_height = lines.len() as u16;
    let padding_top = area.height.saturating_sub(content_height) / 2;

    // 所有行水平居中
    let centered_lines: Vec<Line<'static>> = lines.into_iter().map(|l| l.centered()).collect();

    // 垂直居中：顶部填充空行
    let mut padded_lines: Vec<Line<'static>> = (0..padding_top).map(|_| Line::from("")).collect();
    padded_lines.extend(centered_lines);

    let paragraph = Paragraph::new(Text::from(padded_lines));

    f.render_widget(paragraph, area);
}
