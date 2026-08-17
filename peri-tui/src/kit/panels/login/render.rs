use ratatui_kit::ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

// ── 渲染辅助函数 ──────────────────────────────────────────────────────────────

/// 构建 Login 面板的底部提示行（风格与 setup_wizard 的 make_hint_line 一致）。
pub(super) fn make_hint_line_for_login(
    items: Vec<(String, String)>,
    dim: ratatui::style::Color,
    accent: ratatui::style::Color,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, desc)) in items.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(
            key,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}", desc), Style::default().fg(dim)));
    }
    Line::from(spans)
}

/// 渲染编辑模式下的单行字段（简化自 setup_wizard 的 render_editable_line）。
#[allow(clippy::too_many_arguments)]
pub(super) fn render_login_edit_line(
    label: String,
    display_value: String,
    is_focused: bool,
    cursor_pos: usize,
    cursor_color: ratatui::style::Color,
    dim: ratatui::style::Color,
    text_color: ratatui::style::Color,
    focus_color: ratatui::style::Color,
) -> Line<'static> {
    let (prefix, label_style) = if is_focused {
        (
            "❯ ",
            Style::default()
                .fg(focus_color)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("  ", Style::default().fg(dim))
    };

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(prefix, Style::default().fg(cursor_color)),
        Span::styled(format!("{}: ", label), label_style),
    ];

    if !is_focused {
        spans.push(Span::styled(display_value, Style::default().fg(text_color)));
        return Line::from(spans);
    }

    // 聚焦字段：渲染文本 + 光标
    let chars: Vec<char> = display_value.chars().collect();
    let clamped_cursor = cursor_pos.min(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        if i == clamped_cursor && clamped_cursor < chars.len() {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(text_color)
                    .bg(cursor_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(text_color),
            ));
        }
    }
    if clamped_cursor >= chars.len() {
        spans.push(Span::styled(" ", Style::default().bg(cursor_color)));
    }
    Line::from(spans)
}

/// 将 ProviderConfig::provider_type（"anthropic"|"openai"）映射为 i18n 标签 key。
pub(super) fn provider_type_label(provider_type: &str) -> &'static str {
    match provider_type {
        "anthropic" => "setup-provider-anthropic",
        _ => "setup-provider-openai",
    }
}

/// API Key 脱敏显示：保留最后 4 个字符，其余用 * 代替。
pub(super) fn mask_api_key_display(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if raw.len() <= 4 {
        return "*".repeat(raw.len());
    }
    let tail: String = raw
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}...{}", "*".repeat(4), tail)
}
