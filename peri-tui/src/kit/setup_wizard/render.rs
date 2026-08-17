use crate::app::setup_wizard::*;
use crate::i18n;
use ratatui_kit::ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// ── 辅助渲染函数 ──────────────────────────────────────────────────────────────

fn make_hint_line(items: Vec<(String, String)>, dim: Color, accent: Color) -> Line<'static> {
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

fn render_cursor_items(
    items: Vec<(String, String)>,
    cursor: usize,
    cursor_color: Color,
    text_color: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, (label, desc)) in items.into_iter().enumerate() {
        let is_cursor = i == cursor;
        let c = if is_cursor { "❯" } else { " " };
        let label_style = if is_cursor {
            Style::default()
                .fg(cursor_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_color)
        };
        let detail_style = if is_cursor {
            Style::default().fg(cursor_color)
        } else {
            Style::default().fg(dim)
        };
        let l = label.clone();
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", c), Style::default().fg(cursor_color)),
            Span::styled(format!("{} ", l), label_style),
        ]));
        if !desc.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(desc, detail_style),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}

/// 渲染编辑模式下文本框的内容，带光标指示
#[allow(clippy::too_many_arguments)]
fn render_editable_line(
    label: String,
    display_value: String,
    is_focused: bool,
    cursor_pos: usize,
    cursor_color: Color,
    dim: Color,
    text_color: Color,
    focus_color: Color,
) -> Line<'static> {
    let (_, label_style, prefix) = if is_focused {
        (
            (),
            Style::default()
                .fg(focus_color)
                .add_modifier(Modifier::BOLD),
            "❯ ",
        )
    } else {
        ((), Style::default().fg(dim), "  ")
    };

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(prefix.to_string(), Style::default().fg(cursor_color)),
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
            // 光标在字符上
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

// ── 各步骤渲染 ────────────────────────────────────────────────────────────────

pub(super) fn render_language_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-language-prompt"),
            Style::default().fg(dim),
        )),
        Line::from(""),
    ];

    let items: Vec<(String, String)> = LANGUAGE_OPTIONS
        .iter()
        .map(|(_, name)| (name.to_string(), String::new()))
        .collect();
    lines.extend(render_cursor_items(
        items,
        state.language_cursor,
        cursor_color,
        text_color,
        dim,
    ));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-confirm")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-quit")),
        ],
        dim,
        accent,
    ));

    ("setup-language-title".to_string(), lines)
}

pub(super) fn render_choose_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-choose-provider"),
            Style::default().fg(dim),
        )),
        Line::from(""),
    ];

    let items: Vec<(String, String)> = SetupSource::ALL
        .iter()
        .map(|src| (i18n::tr(src.label()), i18n::tr(src.description())))
        .collect();
    lines.extend(render_cursor_items(
        items,
        state.choose_cursor,
        cursor_color,
        text_color,
        dim,
    ));

    // 迁移失败错误提示
    if let Some(ref err) = state.submit_error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", err),
            Style::default().fg(error_color),
        )));
    }

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-confirm")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-quit")),
        ],
        dim,
        accent,
    ));

    ("setup-welcome-title".to_string(), lines)
}

pub(super) fn render_form_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    focus_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    match state.form_mode {
        FormMode::Browse => {
            let (title, lines) =
                render_browse(state, dim, accent, cursor_color, text_color, error_color);
            (title.to_string(), lines)
        }
        FormMode::Edit => render_edit(
            state,
            dim,
            accent,
            cursor_color,
            text_color,
            focus_color,
            error_color,
        ),
    }
}

fn render_browse(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![Line::from("")];
    let submit_pos = state.providers.len();

    if state.providers.is_empty() {
        lines.push(Line::from(Span::styled(
            i18n::tr("setup-no-providers"),
            Style::default().fg(dim),
        )));
        lines.push(Line::from(""));
    }

    for (idx, mp) in state.providers.iter().enumerate() {
        let is_cursor = idx == state.browse_cursor;
        let cursor = if is_cursor { "❯" } else { " " };
        let check_char = if mp.selected { "✓" } else { " " };
        let check_color = if mp.selected { accent } else { dim };
        let name_style = if is_cursor {
            Style::default()
                .fg(cursor_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_color)
        };
        let detail_style = if is_cursor {
            Style::default().fg(cursor_color)
        } else {
            Style::default().fg(dim)
        };

        let key_summary = if mp.api_key.is_empty() {
            i18n::tr("setup-no-key")
        } else {
            mask_api_key(&mp.api_key)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor), Style::default().fg(cursor_color)),
            Span::styled(
                format!("[{}] ", check_char),
                Style::default().fg(check_color),
            ),
            Span::styled(
                format!("{} ", i18n::tr(mp.provider_type.label())),
                name_style,
            ),
            Span::styled(format!("({}) ", mp.provider_id), Style::default().fg(dim)),
            Span::styled(key_summary, detail_style),
        ]));

        if !mp.base_url.is_empty() {
            let url_text = if mp.base_url.len() > 60 {
                format!("{}...", &mp.base_url[..57])
            } else {
                mp.base_url.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(url_text, Style::default().fg(dim)),
            ]));
        }

        // 显示模型别名（与上方 Provider 信息间空一行）
        lines.push(Line::from(""));
        let alias_labels = [
            i18n::tr("setup-field-fable"),
            i18n::tr("setup-field-opus"),
            i18n::tr("setup-field-sonnet"),
            i18n::tr("setup-field-haiku"),
        ];
        for (ai, label) in alias_labels.iter().enumerate() {
            let model_text = if mp.aliases[ai].len() > 40 {
                format!("{}...", &mp.aliases[ai][..37])
            } else {
                mp.aliases[ai].clone()
            };
            lines.push(Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(format!("{} → ", label), Style::default().fg(dim)),
                Span::styled(model_text, Style::default().fg(accent)),
            ]));
        }

        lines.push(Line::from(""));
    }

    // Submit 错误提示
    if let Some(ref err) = state.submit_error {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", err),
            Style::default().fg(error_color),
        )));
        lines.push(Line::from(""));
    }

    // Submit 按钮
    let submit_active = state.browse_cursor == submit_pos;
    let submit_style = if submit_active {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    let submit_cursor = if submit_active { "❯ " } else { "  " };
    lines.push(Line::from(vec![
        Span::styled(submit_cursor, Style::default().fg(cursor_color)),
        Span::styled(format!(" {}", i18n::tr("setup-submit")), submit_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-edit-submit")),
            ("Space".to_string(), i18n::tr("setup-key-check")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-back")),
        ],
        dim,
        accent,
    ));

    ("setup-configure-title".to_string(), lines)
}

fn render_edit(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    focus_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mp = match state.active_provider_ref() {
        Some(mp) => mp,
        None => {
            return (
                "setup-configure-title".to_string(),
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Internal error: invalid provider index",
                        Style::default().fg(focus_color),
                    )),
                ],
            );
        }
    };

    let header = format!(
        "{} ({})",
        i18n::tr(mp.provider_type.label()),
        mp.provider_id
    );
    let mut lines = vec![Line::from("")];
    let standard_labels = [
        (FormField::ProviderType, i18n::tr("setup-field-type")),
        (FormField::ProviderId, i18n::tr("setup-field-id")),
        (FormField::BaseUrl, i18n::tr("setup-field-base-url")),
        (
            FormField::TestConnectivity,
            i18n::tr("setup-field-test-connectivity"),
        ),
        (FormField::ApiKey, i18n::tr("setup-field-api-key")),
    ];
    let max_label_width = standard_labels
        .iter()
        .map(|(_, l)| l.chars().count())
        .max()
        .unwrap_or(4);
    let pad_width = max_label_width + 2; // 2 for "❯ " prefix

    // ProviderType
    let type_focused = state.form_focus == FormField::ProviderType;
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
            format!(
                "{}{}: ",
                pad_label(&i18n::tr("setup-field-type"), pad_width),
                ""
            ),
            type_style,
        ),
        Span::styled(
            format!("[{}]", i18n::tr(mp.provider_type.label())),
            Style::default().fg(text_color).add_modifier(Modifier::BOLD),
        ),
    ]));

    // ProviderId
    lines.push(render_editable_line(
        i18n::tr("setup-field-id"),
        mp.provider_id.clone(),
        state.form_focus == FormField::ProviderId,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    // BaseUrl
    lines.push(render_editable_line(
        i18n::tr("setup-field-base-url"),
        mp.base_url.clone(),
        state.form_focus == FormField::BaseUrl,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    // TestConnectivity
    let tc_focused = state.form_focus == FormField::TestConnectivity;
    let tc_prefix = if tc_focused { "❯ " } else { "  " };
    let tc_style = if tc_focused {
        Style::default()
            .fg(focus_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    let tc_label = i18n::tr("setup-field-test-connectivity");
    let (tc_status, tc_color) = match &state.connectivity_result {
        Some((true, msg)) => (msg.clone(), accent),
        Some((false, msg)) => (msg.clone(), error_color),
        None => (i18n::tr("setup-key-check"), dim),
    };
    lines.push(Line::from(vec![
        Span::styled(tc_prefix, Style::default().fg(cursor_color)),
        Span::styled(
            format!("{}{}: ", pad_label(&tc_label, pad_width), ""),
            tc_style,
        ),
        Span::styled(tc_status, Style::default().fg(tc_color)),
    ]));

    // ApiKey（脱敏显示）
    let api_display = if mp.api_key.is_empty() {
        String::new()
    } else {
        mask_api_key(&mp.api_key)
    };
    lines.push(render_editable_line(
        i18n::tr("setup-field-api-key"),
        api_display,
        state.form_focus == FormField::ApiKey,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        i18n::tr("setup-model-label"),
        Style::default().fg(dim).add_modifier(Modifier::BOLD),
    )));

    // Fable / Opus / Sonnet / Haiku 模型名
    for (i, field) in [
        FormField::FableModel,
        FormField::OpusModel,
        FormField::SonnetModel,
        FormField::HaikuModel,
    ]
    .iter()
    .enumerate()
    {
        lines.push(render_editable_line(
            i18n::tr(field.i18n_key()),
            mp.aliases[i].clone(),
            state.form_focus == *field,
            state.edit_cursor_pos,
            cursor_color,
            dim,
            text_color,
            focus_color,
        ));
    }

    // Confirm
    let cf_focused = state.form_focus == FormField::Confirm;
    let cf_prefix = if cf_focused { "❯ " } else { "  " };
    let cf_style = if cf_focused {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    lines.push(Line::from(vec![
        Span::styled(cf_prefix, Style::default().fg(cursor_color)),
        Span::styled(format!("  {}", i18n::tr("setup-confirm")), cf_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("←/→/Space".to_string(), i18n::tr("setup-key-switch-type")),
            ("Esc".to_string(), i18n::tr("setup-key-back-list")),
        ],
        dim,
        accent,
    ));

    (header, lines)
}

fn pad_label(label: &str, width: usize) -> String {
    let len = label.chars().count();
    if len >= width - 2 {
        label.to_string()
    } else {
        label.to_string()
    }
}

pub(super) fn render_done_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    _cursor_color: Color,
    text_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-complete-title"),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for mp in &state.providers {
        if !mp.selected {
            continue;
        }
        let provider_id = mp.provider_id.clone();
        let api_key_display = mask_api_key(&mp.api_key);
        let aliases = mp.aliases.clone();
        let type_label = i18n::tr(mp.provider_type.label());
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(accent)),
            Span::styled(format!("{} ", type_label), Style::default().fg(text_color)),
            Span::styled(format!("({})", provider_id), Style::default().fg(dim)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {} ", i18n::tr("setup-label-key")),
                Style::default().fg(dim),
            ),
            Span::styled(api_key_display, Style::default().fg(text_color)),
        ]));
        lines.push(Line::from(""));
        let alias_labels = [
            i18n::tr("setup-field-fable"),
            i18n::tr("setup-field-opus"),
            i18n::tr("setup-field-sonnet"),
            i18n::tr("setup-field-haiku"),
        ];
        for (i, label) in alias_labels.iter().enumerate() {
            let model = aliases[i].clone();
            lines.push(Line::from(vec![
                Span::styled(format!("   {:>6} → ", label), Style::default().fg(dim)),
                Span::styled(model, Style::default().fg(accent)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", i18n::tr("setup-press-enter")),
            Style::default().fg(text_color),
        ),
        Span::styled(
            "Enter",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(i18n::tr("setup-to-start"), Style::default().fg(text_color)),
    ]));

    ("setup-complete-title".to_string(), lines)
}
