use peri_widgets::BorderedPanel;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::setup_wizard::{
        FormField, FormMode, SetupSource, SetupStep, SetupWizardPanel, LANGUAGE_OPTIONS,
    },
    ui::theme,
};

/// Setup 向导全屏渲染入口
pub(crate) fn render_setup_wizard(f: &mut Frame, app: &mut crate::app::App) {
    let area = f.area();

    let step = app.global_ui.setup_wizard.as_ref().unwrap().step;
    match step {
        SetupStep::Choose => {
            let lc = &app.services.lc;
            let wizard = app.global_ui.setup_wizard.as_ref().unwrap();
            render_step_choose(f, wizard, lc, area);
        }
        SetupStep::Language => {
            let lc = &app.services.lc;
            let wizard = app.global_ui.setup_wizard.as_ref().unwrap();
            render_step_language(f, wizard, lc, area);
        }
        SetupStep::Form => render_step_form(f, app, area),
        SetupStep::Done => {
            let lc = &app.services.lc;
            let wizard = app.global_ui.setup_wizard.as_ref().unwrap();
            render_step_done(f, wizard, lc, area);
        }
    }
}

fn render_step_choose(
    f: &mut Frame,
    wizard: &SetupWizardPanel,
    lc: &crate::i18n::LcRegistry,
    area: Rect,
) {
    let inner = BorderedPanel::new(Span::styled(
        lc.tr("setup-welcome-title"),
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::ACCENT))
    .render(f, area);

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            lc.tr("setup-choose-provider"),
            Style::default().fg(theme::MUTED),
        )),
        Line::from(""),
    ];

    for (i, src) in SetupSource::ALL.iter().enumerate() {
        let is_cursor = i == wizard.choose_cursor;
        let cursor_char = if is_cursor { "❯" } else { " " };
        let cursor_style = Style::default().fg(theme::THINKING);
        let label_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD)
        };
        let desc_style = if is_cursor {
            Style::default().fg(theme::THINKING)
        } else {
            Style::default().fg(theme::MUTED)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor_char), cursor_style),
            Span::styled(format!("{} ", src.label(lc)), label_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(src.description(lc), desc_style),
        ]));
        lines.push(Line::from(""));
    }

    lines.push(make_hint_line(vec![
        ("Enter".to_string(), lc.tr("setup-key-confirm")),
        ("↑/↓".to_string(), lc.tr("setup-key-select")),
        ("Esc".to_string(), lc.tr("setup-key-quit")),
    ]));
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_step_language(
    f: &mut Frame,
    wizard: &SetupWizardPanel,
    lc: &crate::i18n::LcRegistry,
    area: Rect,
) {
    let inner = BorderedPanel::new(Span::styled(
        lc.tr("setup-language-title"),
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::ACCENT))
    .render(f, area);

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            lc.tr("setup-language-prompt"),
            Style::default().fg(theme::MUTED),
        )),
        Line::from(""),
    ];

    for (i, (_code, name)) in LANGUAGE_OPTIONS.iter().enumerate() {
        let is_cursor = i == wizard.language_cursor;
        let cursor_char = if is_cursor { "❯" } else { " " };
        let cursor_style = Style::default().fg(theme::THINKING);
        let name_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor_char), cursor_style),
            Span::styled(*name, name_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(make_hint_line(vec![
        ("Enter".to_string(), lc.tr("setup-key-confirm")),
        ("\u{2191}/\u{2193}".to_string(), lc.tr("setup-key-select")),
        ("Esc".to_string(), lc.tr("setup-key-quit")),
    ]));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_step_form(f: &mut Frame, app: &mut crate::app::App, area: Rect) {
    let lc = &app.services.lc;
    let wizard = app.global_ui.setup_wizard.as_mut().unwrap();
    match wizard.form_mode {
        FormMode::Browse => render_form_browse(f, wizard, lc, area),
        FormMode::Edit => render_form_edit(f, wizard, lc, area),
    }
}

/// Browse 模式：只读列表 + Submit
fn render_form_browse(
    f: &mut Frame,
    wizard: &SetupWizardPanel,
    lc: &crate::i18n::LcRegistry,
    area: Rect,
) {
    let inner = BorderedPanel::new(Span::styled(
        lc.tr("setup-configure-title"),
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::ACCENT))
    .render(f, area);

    let mut lines: Vec<Line> = vec![Line::from("")];

    let submit_pos = wizard.providers.len();

    if wizard.providers.is_empty() {
        lines.push(Line::from(Span::styled(
            lc.tr("setup-no-providers"),
            Style::default().fg(theme::MUTED),
        )));
        lines.push(Line::from(""));
    }

    for (idx, mp) in wizard.providers.iter().enumerate() {
        let is_cursor = idx == wizard.browse_cursor;
        let cursor = if is_cursor { "❯" } else { " " };
        let check_char = if mp.selected { "✓" } else { " " };
        let check_color = if mp.selected { theme::SAGE } else { theme::DIM };
        let name_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let detail_style = if is_cursor {
            Style::default().fg(theme::THINKING)
        } else {
            Style::default().fg(theme::MUTED)
        };

        let key_summary = if mp.field_api_key.is_empty() {
            lc.tr("setup-no-key")
        } else {
            mask_api_key(&mp.field_api_key.value())
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor), Style::default().fg(theme::THINKING)),
            Span::styled(
                format!("[{}] ", check_char),
                Style::default().fg(check_color),
            ),
            Span::styled(format!("{} ", mp.provider_type.label(lc)), name_style),
            Span::styled(
                format!("({}) ", mp.field_provider_id.value()),
                Style::default().fg(theme::MUTED),
            ),
            Span::styled(key_summary, detail_style),
        ]));

        // 第二行：base_url 摘要
        if !mp.field_base_url.is_empty() {
            let url_style = Style::default().fg(theme::DIM);
            lines.push(Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(mp.field_base_url.value(), url_style),
            ]));
        }

        lines.push(Line::from(""));
    }

    // Submit 错误提示
    if let Some(ref err) = wizard.submit_error {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", err),
            Style::default().fg(theme::WARNING),
        )));
        lines.push(Line::from(""));
    }

    // Submit 按钮
    let submit_active = wizard.browse_cursor == submit_pos;
    let submit_style = if submit_active {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let submit_cursor = if submit_active { "❯ " } else { "  " };
    lines.push(Line::from(vec![
        Span::styled(submit_cursor, Style::default().fg(theme::THINKING)),
        Span::styled(format!(" {}", lc.tr("setup-submit")), submit_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(vec![
        ("Enter".to_string(), lc.tr("setup-key-edit-submit")),
        ("Space".to_string(), lc.tr("setup-key-check")),
        ("↑/↓".to_string(), lc.tr("setup-key-select")),
        ("Esc".to_string(), lc.tr("setup-key-back")),
    ]));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Edit 模式：编辑单个 provider 的所有字段
fn render_form_edit(
    f: &mut Frame,
    wizard: &mut SetupWizardPanel,
    lc: &crate::i18n::LcRegistry,
    area: Rect,
) {
    let mp = match wizard.providers.get(wizard.active_provider) {
        Some(provider) => provider,
        None => {
            let inner = BorderedPanel::new(Span::styled(
                lc.tr("setup-configure-title"),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
            .border_style(Style::default().fg(theme::ACCENT))
            .render(f, area);
            f.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Internal error: invalid provider index",
                        Style::default().fg(theme::WARNING),
                    )),
                ])),
                inner,
            );
            return;
        }
    };
    let header = lc.tr_args(
        "setup-edit-title",
        &[
            ("type".into(), mp.provider_type.label(lc).into()),
            ("id".into(), mp.field_provider_id.value().into()),
        ],
    );

    let inner = BorderedPanel::new(Span::styled(
        header,
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::ACCENT))
    .render(f, area);

    let mut lines: Vec<Line> = vec![Line::from("")];

    // 动态计算标准字段 label 的最大显示列宽（替代硬编码 4），
    // 确保 overlay x 偏移与底层 Paragraph 中 label 的实际渲染宽度一致。
    // pad_display_columns 不截断——只用 max 保证较短 label 被填充到相同宽度，
    // 超长 label 的宽度会自然成为新 max。
    let standard_labels = [
        lc.tr("setup-field-type"),
        lc.tr("setup-field-id"),
        lc.tr("setup-field-base-url"),
        lc.tr("setup-field-api-key"),
    ];
    let max_standard_label_width: usize = standard_labels
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.as_str()))
        .max()
        .unwrap_or(4);
    let label_prefix_width: usize = 2 + max_standard_label_width + 1; // "❯ " + label + " "
    let mut overlay_line: Option<(usize, usize)> = None;

    lines.push(render_field_line(
        &lc.tr("setup-field-type"),
        max_standard_label_width,
        FormField::ProviderType,
        format!("[{}]", mp.provider_type.label(lc)),
        wizard.form_focus,
    ));

    lines.push(render_field_line(
        &lc.tr("setup-field-id"),
        max_standard_label_width,
        FormField::ProviderId,
        if wizard.form_focus == FormField::ProviderId {
            String::new()
        } else {
            mp.field_provider_id.value()
        },
        wizard.form_focus,
    ));
    if wizard.form_focus == FormField::ProviderId {
        overlay_line = Some((lines.len() - 1, label_prefix_width));
    }

    lines.push(render_field_line(
        &lc.tr("setup-field-base-url"),
        max_standard_label_width,
        FormField::BaseUrl,
        if wizard.form_focus == FormField::BaseUrl {
            String::new()
        } else {
            mp.field_base_url.value()
        },
        wizard.form_focus,
    ));
    if wizard.form_focus == FormField::BaseUrl {
        overlay_line = Some((lines.len() - 1, label_prefix_width));
    }

    // /v1 suffix hint
    lines.push(Line::from(Span::styled(
        format!("  ({})", lc.tr("setup-hint-base-url-v1")),
        Style::default().fg(theme::DIM),
    )));

    // 测试联通性按钮 + 结果
    let test_active = wizard.form_focus == FormField::TestConnectivity;
    let test_cursor = if test_active { "❯ " } else { "  " };
    let test_style = if test_active {
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    lines.push(Line::from(vec![
        Span::styled(test_cursor, Style::default().fg(theme::THINKING)),
        Span::styled(format!(" {}", lc.tr("setup-test-connectivity")), test_style),
    ]));
    if let Some((ok, ref msg)) = wizard.connectivity_result {
        let result_color = if ok { theme::SAGE } else { theme::WARNING };
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(result_color),
        )));
    }

    // API Key: 非聚焦时显示 masked，聚焦时 overlay textarea
    let key_display = if mp.field_api_key.is_empty() {
        String::new()
    } else {
        "•".repeat(mp.field_api_key.value().chars().count())
    };
    lines.push(render_field_line(
        &lc.tr("setup-field-api-key"),
        max_standard_label_width,
        FormField::ApiKey,
        if wizard.form_focus == FormField::ApiKey {
            String::new()
        } else {
            key_display
        },
        wizard.form_focus,
    ));
    if wizard.form_focus == FormField::ApiKey {
        overlay_line = Some((lines.len() - 1, label_prefix_width));
    }

    lines.push(Line::from(Span::styled(
        "  ─────────────────────────────────",
        Style::default().fg(theme::DIM),
    )));

    let alias_labels = [
        (lc.tr("setup-field-opus"), FormField::OpusModel, 0),
        (lc.tr("setup-field-sonnet"), FormField::SonnetModel, 1),
        (lc.tr("setup-field-haiku"), FormField::HaikuModel, 2),
    ];
    for (label, field, ai) in alias_labels {
        let alias_prefix = format!("{} {} ", label, lc.tr("setup-model-label"));
        let alias_label_width = UnicodeWidthStr::width(alias_prefix.as_str());
        let alias_prefix_width = 2 + alias_label_width + 1;
        lines.push(render_field_line(
            &alias_prefix,
            alias_label_width,
            field,
            if wizard.form_focus == field {
                String::new()
            } else {
                mp.aliases[ai].field_model_id.value()
            },
            wizard.form_focus,
        ));
        if wizard.form_focus == field {
            overlay_line = Some((lines.len() - 1, alias_prefix_width));
        }
    }

    // Confirm 按钮
    let confirm_active = wizard.form_focus == FormField::Confirm;
    let confirm_style = if confirm_active {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let confirm_cursor = if confirm_active { "❯ " } else { "  " };
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(confirm_cursor, Style::default().fg(theme::THINKING)),
        Span::styled(format!(" {}", lc.tr("setup-confirm")), confirm_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(vec![
        ("Enter".to_string(), lc.tr("setup-key-confirm")),
        ("←/→".to_string(), lc.tr("setup-key-switch-type")),
        ("Esc".to_string(), lc.tr("setup-key-back-list")),
    ]));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Overlay textarea on the active field line
    if let Some((line_idx, x_offset)) = overlay_line {
        if (line_idx as u16) < inner.height {
            let mp = &mut wizard.providers[wizard.active_provider];
            let field: Option<&mut crate::app::FieldTextarea> = match wizard.form_focus {
                FormField::ProviderId => Some(&mut mp.field_provider_id),
                FormField::BaseUrl => Some(&mut mp.field_base_url),
                FormField::ApiKey => Some(&mut mp.field_api_key),
                FormField::OpusModel => Some(&mut mp.aliases[0].field_model_id),
                FormField::SonnetModel => Some(&mut mp.aliases[1].field_model_id),
                FormField::HaikuModel => Some(&mut mp.aliases[2].field_model_id),
                _ => None,
            };
            if let Some(field) = field {
                let y = inner.y + line_idx as u16;
                let x = inner.x + x_offset as u16;
                let textarea_area = Rect {
                    x,
                    y,
                    width: inner.width.saturating_sub(x_offset as u16),
                    height: 1,
                };
                field.render(f, textarea_area);
            }
        }
    }
}

/// 渲染单个字段行（带光标指示器、标签固定宽度右对齐）
fn render_field_line(
    label: &str,
    label_width: usize,
    field: FormField,
    value: String,
    focus: FormField,
) -> Line<'static> {
    let is_active = focus == field;
    let cursor = if is_active { "❯ " } else { "  " };
    let lbl = if is_active {
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let val = if is_active {
        Style::default().fg(theme::THINKING)
    } else {
        Style::default().fg(theme::TEXT)
    };
    let padded = pad_display_columns(label, label_width);
    Line::from(vec![
        Span::styled(cursor, Style::default().fg(theme::THINKING)),
        Span::styled(padded, lbl),
        Span::styled(format!(" {}", value), val),
    ])
}

/// 将字符串右对齐填充到指定 Unicode 显示列宽
fn pad_display_columns(s: &str, target_cols: usize) -> String {
    let cols: usize = s
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    if cols >= target_cols {
        s.to_string()
    } else {
        let padding = target_cols - cols;
        format!("{}{}", s, " ".repeat(padding))
    }
}

fn render_step_done(
    f: &mut Frame,
    wizard: &SetupWizardPanel,
    lc: &crate::i18n::LcRegistry,
    area: Rect,
) {
    let inner = BorderedPanel::new(Span::styled(
        lc.tr("setup-complete-title"),
        Style::default()
            .fg(theme::SAGE)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::SAGE))
    .render(f, area);

    let mut lines = vec![Line::from("")];

    let selected: Vec<_> = wizard.providers.iter().filter(|p| p.selected).collect();
    for mp in &selected {
        lines.push(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(theme::SAGE)),
            Span::styled(
                format!("{} ", mp.provider_type.label(lc)),
                Style::default().fg(theme::TEXT),
            ),
            Span::styled(
                format!("({})", mp.field_provider_id.value()),
                Style::default().fg(theme::MUTED),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {} ", lc.tr("setup-label-key")),
                Style::default().fg(theme::MUTED),
            ),
            Span::styled(
                mask_api_key(&mp.field_api_key.value()),
                Style::default().fg(theme::TEXT),
            ),
        ]));
        let alias_labels = [
            lc.tr("setup-field-opus"),
            lc.tr("setup-field-sonnet"),
            lc.tr("setup-field-haiku"),
        ];
        for (i, label) in alias_labels.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:>6} → ", label),
                    Style::default().fg(theme::MUTED),
                ),
                Span::styled(
                    mp.aliases[i].field_model_id.value(),
                    Style::default().fg(theme::ACCENT),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", lc.tr("setup-press-enter")),
            Style::default().fg(theme::TEXT),
        ),
        Span::styled(
            "Enter",
            Style::default()
                .fg(theme::SAGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(lc.tr("setup-to-start"), Style::default().fg(theme::TEXT)),
    ]));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// 生成底部快捷键提示行
fn make_hint_line(items: Vec<(String, String)>) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, (key, desc)) in items.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(
            key,
            Style::default()
                .fg(theme::WARNING)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(desc, Style::default().fg(theme::MUTED)));
    }
    Line::from(spans)
}

/// API Key 脱敏
fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 8 {
        "•".repeat(len)
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[len - 4..].iter().collect();
        format!("{}••••{}", prefix, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{setup_wizard::SetupWizardPanel, App};
    include!("setup_wizard_test.rs");
}
