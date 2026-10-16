use crate::i18n;

use super::{LoginEditField, LoginEditState};
use ratatui_kit::ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// ── 渲染辅助函数 ──────────────────────────────────────────────────────────────

/// Edit 视图配色（由调用方从当前主题派生；渲染辅助函数不读全局状态）。
pub(super) struct LoginRenderColors {
    pub(super) cursor: Color,
    pub(super) dim: Color,
    pub(super) text: Color,
    pub(super) focus: Color,
}

/// 构建 Edit 视图的行（纯推导，命中不涉及本视图）。
pub(super) fn build_edit_lines(
    es: &LoginEditState,
    focus: LoginEditField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let LoginRenderColors {
        cursor: cursor_color,
        dim,
        text: text_color,
        focus: focus_color,
    } = *colors;
    let ef = focus;
    let ec = cursor_pos;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let is_new = es.original_provider_id.is_empty();
    let title_key = if is_new {
        "login-panel-title-new"
    } else {
        "login-panel-title-edit"
    };
    let title_text = if is_new {
        format!("  {}", i18n::tr(title_key))
    } else {
        format!("  {}: {}", i18n::tr(title_key), es.original_provider_id)
    };
    lines.push(Line::from(vec![Span::styled(
        title_text,
        Style::new().fg(focus_color).bold(),
    )]));
    lines.push(Line::from(""));

    // ── 标准字段：Type（toggle）+ ID / BaseUrl / ApiKey（文本输入）
    // 布局与 setup_wizard 的 render_edit 一致：focus 时 ❯ + focus 色 label
    let type_focused = ef == LoginEditField::ProviderType;
    let type_prefix = if type_focused { "❯ " } else { "  " };
    let type_style = if type_focused {
        Style::default()
            .fg(focus_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    lines.push(Line::from(vec![
        Span::styled(type_prefix, Style::default().fg(cursor_color)),
        Span::styled(
            format!("{}: ", i18n::tr(LoginEditField::ProviderType.i18n_key())),
            type_style,
        ),
        Span::styled(
            format!("[{}]", i18n::tr(provider_type_label(&es.provider_type))),
            Style::default().fg(text_color).add_modifier(Modifier::BOLD),
        ),
    ]));

    for field in &[
        LoginEditField::ProviderId,
        LoginEditField::BaseUrl,
        LoginEditField::ApiKey,
    ] {
        let is_focused = *field == ef;
        let display_val = if *field == LoginEditField::ApiKey {
            // API Key 脱敏显示（编辑时也只显示脱敏版本 + 实际输入体现在光标位置）
            mask_api_key_display(es.field_value(*field))
        } else {
            es.field_value(*field).to_string()
        };
        lines.push(render_login_edit_line(
            i18n::tr(field.i18n_key()),
            display_val,
            is_focused,
            ec,
            cursor_color,
            dim,
            text_color,
            focus_color,
        ));
    }

    // ── Model 分组标题（与 setup_wizard 一致）
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        i18n::tr("login-model-label"),
        Style::default().fg(dim).add_modifier(Modifier::BOLD),
    )));

    // ── Fable / Opus / Sonnet / Haiku 模型名
    for field in &[
        LoginEditField::FableModel,
        LoginEditField::OpusModel,
        LoginEditField::SonnetModel,
        LoginEditField::HaikuModel,
    ] {
        lines.push(render_login_edit_line(
            i18n::tr(field.i18n_key()),
            es.field_value(*field).to_string(),
            *field == ef,
            ec,
            cursor_color,
            dim,
            text_color,
            focus_color,
        ));
    }

    // ── 确认按钮（参考 setup_wizard 的 Confirm 行：focus 时 ❯ + 强调色）
    let cf_focused = ef == LoginEditField::Confirm;
    let cf_prefix = if cf_focused { "❯ " } else { "  " };
    let cf_style = if cf_focused {
        Style::default()
            .fg(cursor_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(cf_prefix, Style::default().fg(cursor_color)),
        Span::styled(format!("  {}", i18n::tr("login-confirm")), cf_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line_for_login(
        vec![
            (
                "\u{2191}/\u{2193}".to_string(),
                i18n::tr("hint-login-field"),
            ),
            (
                "\u{2190}/\u{2192}/Space".to_string(),
                i18n::tr("hint-login-toggle"),
            ),
            ("Enter".to_string(), i18n::tr("hint-login-confirm")),
            ("Esc".to_string(), i18n::tr("hint-login-back")),
        ],
        dim,
        focus_color,
    ));

    lines
}

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
