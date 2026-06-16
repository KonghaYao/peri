use peri_widgets::BorderedPanel;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{
        config_panel::{
            ConfigPanel, ROW_AUTOCOMPACT, ROW_COUNT, ROW_DIFF, ROW_GENERAL_HEADER, ROW_LANGUAGE,
            ROW_OVERRIDES_HEADER, ROW_PERSONA, ROW_PROACTIVENESS, ROW_SEPARATOR, ROW_STREAMING,
            ROW_THRESHOLD, ROW_TONE,
        },
        App,
    },
    ui::theme,
};
use unicode_width::UnicodeWidthStr;

/// 行号 → i18n 字段标签键
fn field_label_key(row: usize) -> &'static str {
    match row {
        ROW_AUTOCOMPACT => "config-field-autocompact",
        ROW_THRESHOLD => "config-field-compact-threshold",
        ROW_LANGUAGE => "config-field-language",
        ROW_DIFF => "config-field-diff",
        ROW_STREAMING => "config-field-streaming",
        ROW_PERSONA => "config-field-persona",
        ROW_TONE => "config-field-tone",
        ROW_PROACTIVENESS => "config-field-proactiveness",
        _ => "???",
    }
}

/// 语言代码 → 显示名（不需要 i18n，语言名本身就是自描述的）
fn lang_display(code: &str) -> &str {
    match code {
        "en" => "English",
        "zh-CN" => "简体中文",
        _ => "auto",
    }
}

/// /config 面板渲染（单一直接编辑模式）
pub(crate) fn render_config_panel(
    f: &mut Frame,
    panel: &mut ConfigPanel,
    app: &mut App,
    area: Rect,
) {
    let lc = &app.services.lc;

    let title = lc.tr("config-panel-title");

    let inner = BorderedPanel::new(Span::styled(
        title,
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::BORDER))
    .render(f, area);

    app.session_mgr.current_mut().ui.panel_area = Some(inner);

    let mut lines: Vec<Line> = Vec::new();
    let mut active_textarea_overlay: Option<u16> = None;

    // 动态计算所有字段 label 的最大显示列宽度（替代硬编码 14）。
    // Rust format! 的 width 参数按字符数填充，CJK 字符 1 char = 2 显示列，
    // 必须以显示列为准计算 overlay x 偏移，避免文本错位。
    let label_display_widths: Vec<usize> = (0..ROW_COUNT)
        .filter_map(|row| {
            let key = field_label_key(row);
            if key == "???" {
                None
            } else {
                Some(UnicodeWidthStr::width(lc.tr(key).as_str()))
            }
        })
        .collect();
    let max_label_display_width = label_display_widths.iter().max().copied().unwrap_or(14);
    let label_column_width = max_label_display_width; // 用于 format!("{:<width$}")

    for row in 0..ROW_COUNT {
        match row {
            ROW_GENERAL_HEADER => {
                lines.push(Line::from(vec![Span::styled(
                    lc.tr("config-group-general"),
                    Style::default()
                        .fg(theme::SAGE)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            ROW_SEPARATOR => {
                lines.push(Line::from(""));
            }
            ROW_OVERRIDES_HEADER => {
                lines.push(Line::from(vec![Span::styled(
                    lc.tr("config-group-prompt-overrides"),
                    Style::default()
                        .fg(theme::SAGE)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            ROW_AUTOCOMPACT => {
                let is_active = panel.cursor == row;
                let label_style = active_or_text(is_active);
                let active_style = Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD);
                let inactive_style = Style::default().fg(theme::MUTED);
                let desc_style = Style::default().fg(theme::MUTED);

                let on_span = if panel.buf_autocompact {
                    Span::styled(format!("[{}]", lc.tr("config-value-on")), active_style)
                } else {
                    Span::styled(lc.tr("config-value-on"), inactive_style)
                };
                let off_span = if panel.buf_autocompact {
                    Span::styled(lc.tr("config-value-off"), inactive_style)
                } else {
                    Span::styled(format!("[{}]", lc.tr("config-value-off")), active_style)
                };

                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                    on_span,
                    Span::styled("  ", Style::default()),
                    off_span,
                ]));
                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr("config-desc-autocompact")),
                    desc_style,
                )));
            }
            ROW_LANGUAGE => {
                let is_active = panel.cursor == row;
                let label_style = active_or_text(is_active);
                let active_style = Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD);
                let inactive_style = Style::default().fg(theme::MUTED);
                let desc_style = Style::default().fg(theme::MUTED);

                let options = ["en", "zh-CN"];
                let mut value_spans: Vec<Span> = Vec::new();
                for (i, code) in options.iter().enumerate() {
                    let display = lang_display(code);
                    let is_selected = *code == panel.buf_language
                        || (code.is_empty() && panel.buf_language.is_empty());
                    if is_selected {
                        value_spans.push(Span::styled(format!("[{}]", display), active_style));
                    } else {
                        value_spans.push(Span::styled(display.to_string(), inactive_style));
                    }
                    if i < options.len() - 1 {
                        value_spans.push(Span::styled("  ", Style::default()));
                    }
                }
                let mut line_spans = vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                ];
                line_spans.extend(value_spans);
                lines.push(Line::from(line_spans));
                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr("config-desc-language")),
                    desc_style,
                )));
            }
            ROW_DIFF => {
                let is_active = panel.cursor == row;
                let label_style = active_or_text(is_active);
                let active_style = Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD);
                let inactive_style = Style::default().fg(theme::MUTED);
                let desc_style = Style::default().fg(theme::MUTED);

                let on_span = if panel.buf_diff {
                    Span::styled(format!("[{}]", lc.tr("config-value-on")), active_style)
                } else {
                    Span::styled(lc.tr("config-value-on"), inactive_style)
                };
                let off_span = if panel.buf_diff {
                    Span::styled(lc.tr("config-value-off"), inactive_style)
                } else {
                    Span::styled(format!("[{}]", lc.tr("config-value-off")), active_style)
                };

                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                    on_span,
                    Span::styled("  ", Style::default()),
                    off_span,
                ]));
                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr("config-desc-diff")),
                    desc_style,
                )));
            }
            ROW_STREAMING => {
                let is_active = panel.cursor == row;
                let label_style = active_or_text(is_active);
                let active_style = Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD);
                let inactive_style = Style::default().fg(theme::MUTED);
                let desc_style = Style::default().fg(theme::MUTED);

                let vals = ["streaming", "block", "none"];
                let mut value_spans: Vec<Span> = Vec::new();
                for (i, v) in vals.iter().enumerate() {
                    if *v == panel.buf_streaming.as_str() {
                        value_spans.push(Span::styled(format!("[{}]", v), active_style));
                    } else {
                        value_spans.push(Span::styled(v.to_string(), inactive_style));
                    }
                    if i < vals.len() - 1 {
                        value_spans.push(Span::styled("  ", Style::default()));
                    }
                }
                let mut line_spans = vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                ];
                line_spans.extend(value_spans);
                lines.push(Line::from(line_spans));
                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr("config-desc-streaming")),
                    desc_style,
                )));
            }
            ROW_PROACTIVENESS => {
                let is_active = panel.cursor == row;
                let label_style = active_or_text(is_active);
                let active_style = Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD);
                let inactive_style = Style::default().fg(theme::MUTED);
                let desc_style = Style::default().fg(theme::MUTED);

                let vals = ["low", "medium", "high"];
                let mut value_spans: Vec<Span> = Vec::new();
                for (i, v) in vals.iter().enumerate() {
                    if *v == panel.buf_proactiveness.as_str() {
                        value_spans.push(Span::styled(format!("[{}]", v), active_style));
                    } else {
                        value_spans.push(Span::styled(v.to_string(), inactive_style));
                    }
                    if i < vals.len() - 1 {
                        value_spans.push(Span::styled("  ", Style::default()));
                    }
                }
                let mut line_spans = vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                ];
                line_spans.extend(value_spans);
                lines.push(Line::from(line_spans));
                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr("config-desc-proactiveness")),
                    desc_style,
                )));
            }
            ROW_THRESHOLD | ROW_PERSONA | ROW_TONE => {
                let is_active = panel.cursor == row;
                let desc_key = match row {
                    ROW_THRESHOLD => "config-desc-threshold",
                    ROW_PERSONA => "config-desc-persona",
                    ROW_TONE => "config-desc-tone",
                    _ => "",
                };

                let field = match row {
                    ROW_THRESHOLD => &panel.field_threshold,
                    ROW_PERSONA => &panel.field_persona,
                    ROW_TONE => &panel.field_tone,
                    _ => unreachable!(),
                };

                let label_style = active_or_text(is_active);
                let value_style = Style::default().fg(theme::TEXT);
                let desc_style = Style::default().fg(theme::MUTED);

                let value_display = if is_active {
                    String::new()
                } else if field.is_empty() {
                    "-".to_string()
                } else {
                    field.value()
                };

                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            lc.tr(field_label_key(row)),
                            width = label_column_width
                        ),
                        label_style,
                    ),
                    Span::styled(" ", Style::default()),
                    Span::styled(value_display, value_style),
                ]));

                // 记录活跃 textarea 的行索引，用于 overlay 渲染
                if is_active {
                    active_textarea_overlay = Some(lines.len() as u16 - 1);
                }

                lines.push(Line::from(Span::styled(
                    format!("      {}", lc.tr(desc_key)),
                    desc_style,
                )));
            }
            _ => {}
        }
    }

    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    // overlay 活跃 textarea
    if let Some(line_idx) = active_textarea_overlay {
        if line_idx < inner.height {
            // "  " (2 cols) + label (display cols) + " " (1 col)
            let label_width: u16 = (2 + label_column_width + 1) as u16;
            let value_area = Rect {
                x: inner.x + label_width,
                y: inner.y + line_idx,
                width: inner.width.saturating_sub(label_width),
                height: 1,
            };
            if value_area.width > 0 {
                if let Some(field) = panel.active_field() {
                    field.render(f, value_area);
                }
            }
        }
    }
}

fn active_or_text(is_active: bool) -> Style {
    if is_active {
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT)
    }
}
