use peri_theme::{bridge::ThemeDefinitionExt, theme::ThemeDefinition};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use ratatui_kit::{ComponentTheme, components::TableTheme};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::kit::markdown::{GenUiSegment, TableData, table_data_to_lines};

use super::{GenUiButton, GenUiButtonInfo};

#[derive(Debug, Default)]
pub struct GenUiRender {
    pub lines: Vec<Line<'static>>,
    pub buttons: Vec<GenUiButtonInfo>,
}

impl GenUiRender {
    fn append(&mut self, mut other: Self) {
        let offset = self.lines.len();
        for button in &mut other.buttons {
            button.line_start = button.line_start.saturating_add(offset);
            button.line_end = button.line_end.saturating_add(offset);
        }
        self.lines.extend(other.lines);
        self.buttons.extend(other.buttons);
    }
}

pub fn render(segment: &GenUiSegment, width: usize, theme: &ThemeDefinition) -> GenUiRender {
    if width == 0 {
        return GenUiRender::default();
    }
    let width = width.min(4096);
    let block = segment.block_id();
    let updated = super::view_state::context(block).map(|context| segment.with_context(&context));
    let segment = updated.as_ref().unwrap_or(segment);
    let mut remaining = 100_000;
    let mut output = segment
        .root
        .as_ref()
        .map(|root| element(root, width, theme, 0, &mut remaining))
        .unwrap_or_default();
    for error in &segment.errors {
        output.append(muted_error(error, width, theme));
    }
    for button in &mut output.buttons {
        button.block = Some(block);
        button.parsed = Some(std::sync::Arc::clone(&segment.parsed));
    }
    output
}

fn element(
    value: &Value,
    width: usize,
    theme: &ThemeDefinition,
    depth: usize,
    remaining: &mut usize,
) -> GenUiRender {
    if depth >= 64 || *remaining == 0 {
        return muted_error("component tree exceeds rendering limit", width, theme);
    }
    *remaining -= 1;
    if value.is_null() {
        return GenUiRender::default();
    }
    let Some(props) = value.get("props").and_then(Value::as_object) else {
        return muted_error("invalid component", width, theme);
    };
    let name = value
        .get("typeName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = |key: &str| props.get(key).and_then(Value::as_str).unwrap_or_default();
    let body = Style::default().fg(theme.component.markdown.text);
    let heading = Style::default()
        .fg(theme.semantic.accent)
        .add_modifier(Modifier::BOLD);
    match name {
        "Stack" | "Card" => {
            let framed = name == "Card" && width >= 6;
            let inner = if framed { width - 4 } else { width };
            let children = props
                .get("children")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let row = name == "Stack" && text("direction") == "row" && children.len() > 1;
            let column_width =
                inner.saturating_sub(children.len().saturating_sub(1) * 2) / children.len().max(1);
            // 按钮保持纵向布局，避免窄视口折行后热区跨列重叠。
            let horizontal = row && column_width >= 8 && !contains_button(value, 0);
            let mut content = GenUiRender::default();
            let mut columns = Vec::new();
            for child in children {
                let rendered = element(
                    child,
                    if horizontal { column_width } else { inner },
                    theme,
                    depth + 1,
                    remaining,
                );
                if horizontal {
                    columns.push(rendered);
                } else {
                    content.append(rendered);
                }
            }
            if horizontal {
                content = join_columns(columns, column_width);
            }
            if name == "Card" {
                if !text("title").is_empty() {
                    let mut titled = plain(text("title"), inner, heading);
                    titled.append(content);
                    content = titled;
                }
                if framed {
                    content = frame(content, width, theme);
                }
            }
            content
        }
        "Text" => {
            let style = match text("variant") {
                "muted" | "caption" => body.fg(theme.semantic.text.muted),
                "heading" | "bold" => body.add_modifier(Modifier::BOLD),
                "code" => body.fg(theme.component.markdown.code),
                _ => body,
            };
            plain(text("content"), width, style)
        }
        "Heading" => {
            let level = props.get("level").and_then(Value::as_u64).unwrap_or(1);
            plain(
                text("text"),
                width,
                if level <= 2 {
                    heading
                } else {
                    body.add_modifier(Modifier::BOLD)
                },
            )
        }
        "List" => {
            let mut output = GenUiRender::default();
            if let Some(items) = props.get("items").and_then(Value::as_array) {
                for (index, item) in items.iter().enumerate() {
                    let prefix = if props
                        .get("ordered")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        format!("{}. ", index + 1)
                    } else {
                        "• ".into()
                    };
                    let text = display_value(item);
                    output.append(plain(&format!("{prefix}{text}"), width, body));
                }
            }
            output
        }
        "Table" => table(props, width, theme),
        "KeyValue" => {
            let mut output = GenUiRender::default();
            if !text("title").is_empty() {
                output.append(plain(text("title"), width, heading));
            }
            if let Some(items) = props.get("items").and_then(Value::as_array) {
                for item in items {
                    let key = item.get("key").and_then(Value::as_str).unwrap_or_default();
                    let value = item.get("value").map(display_value).unwrap_or_default();
                    output.append(plain(&format!("{key}: {value}"), width, body));
                }
            }
            output
        }
        "Progress" => progress(props, width, theme),
        "Divider" => GenUiRender {
            lines: vec![Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(theme.semantic.border.dim),
            ))],
            buttons: vec![],
        },
        "Button" => button(props, width, theme),
        _ => muted_error(&format!("unsupported component: {name}"), width, theme),
    }
}

fn contains_button(value: &Value, depth: usize) -> bool {
    if depth >= 64 {
        return true;
    }
    if value.get("typeName").and_then(Value::as_str) == Some("Button") {
        return true;
    }
    value
        .get("props")
        .and_then(|props| props.get("children"))
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children
                .iter()
                .any(|child| contains_button(child, depth + 1))
        })
}

fn join_columns(columns: Vec<GenUiRender>, width: usize) -> GenUiRender {
    let height = columns
        .iter()
        .map(|column| column.lines.len())
        .max()
        .unwrap_or(0);
    let mut output = GenUiRender::default();
    for row in 0..height {
        let mut spans = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let line = column.lines.get(row).cloned().unwrap_or_default();
            let padding = width.saturating_sub(line.width());
            spans.extend(line.spans);
            spans.push(Span::raw(" ".repeat(padding)));
        }
        output.lines.push(Line::from(spans));
    }
    output
}

fn frame(mut content: GenUiRender, width: usize, theme: &ThemeDefinition) -> GenUiRender {
    let border = Style::default().fg(theme.semantic.border.default);
    let mut lines = vec![Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(width - 2)),
        border,
    ))];
    for line in content.lines {
        let padding = width.saturating_sub(4).saturating_sub(line.width());
        let mut spans = vec![Span::styled("│ ", border)];
        spans.extend(line.spans);
        spans.push(Span::styled(format!("{} │", " ".repeat(padding)), border));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(width - 2)),
        border,
    )));
    for button in &mut content.buttons {
        button.line_start += 1;
        button.line_end += 1;
        button.x_start = button.x_start.saturating_add(2);
        button.x_end = button.x_end.saturating_add(2);
    }
    GenUiRender {
        lines,
        buttons: content.buttons,
    }
}

fn button(
    props: &serde_json::Map<String, Value>,
    width: usize,
    theme: &ThemeDefinition,
) -> GenUiRender {
    let descriptor = GenUiButton::from_props(props);
    let action = match descriptor.action {
        Ok(action) => action,
        Err(error) => return muted_error(&error, width, theme),
    };
    if width < 4 {
        return muted_error("button needs at least four columns", width, theme);
    }
    let label = fit(&sanitize(&descriptor.label).replace('\n', " "), width - 4);
    let primary = props.get("variant").and_then(Value::as_str) == Some("primary");
    let style = Style::default().fg(theme.semantic.accent).bg(if primary {
        theme.semantic.surface.raised
    } else {
        theme.semantic.surface.default
    });
    GenUiRender {
        lines: vec![Line::from(Span::styled(format!("[ {label} ]"), style))],
        buttons: vec![GenUiButtonInfo {
            line_start: 0,
            line_end: 1,
            x_start: 0,
            x_end: label.width().saturating_add(4).min(u16::MAX as usize) as u16,
            block: None,
            parsed: None,
            action,
            label: descriptor.label,
        }],
    }
}

fn progress(
    props: &serde_json::Map<String, Value>,
    width: usize,
    theme: &ThemeDefinition,
) -> GenUiRender {
    let value = props.get("value").and_then(Value::as_f64).unwrap_or(0.0);
    let max = props.get("max").and_then(Value::as_f64).unwrap_or(100.0);
    if !value.is_finite() || !max.is_finite() || max <= 0.0 {
        return muted_error("invalid progress range", width, theme);
    }
    let ratio = (value / max).clamp(0.0, 1.0);
    let label = props
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut output = plain(
        &format!("{label} {:.0}%", ratio * 100.0),
        width,
        Style::default().fg(theme.component.markdown.text),
    );
    if width >= 4 {
        let filled = ((width - 2) as f64 * ratio).round() as usize;
        output.lines.push(Line::from(vec![
            Span::styled("[", Style::default().fg(theme.semantic.border.default)),
            Span::styled(
                "━".repeat(filled),
                Style::default().fg(theme.semantic.accent),
            ),
            Span::styled(
                "─".repeat(width - 2 - filled),
                Style::default().fg(theme.semantic.border.dim),
            ),
            Span::styled("]", Style::default().fg(theme.semantic.border.default)),
        ]));
    }
    output
}

fn table(
    props: &serde_json::Map<String, Value>,
    width: usize,
    theme: &ThemeDefinition,
) -> GenUiRender {
    let columns = props
        .get("columns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let rows = props
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // 既有表格适配器内部使用 u16 几何，先限制模型输入，避免缓冲区面积及高度溢出。
    if columns.is_empty()
        || columns.len() > 32
        || columns.len() > width.saturating_sub(1) / 4
        || rows.len() > 256
    {
        return muted_error(
            "table dimensions exceed available rendering space",
            width,
            theme,
        );
    }
    let mut bytes = 0usize;
    let headers: Vec<_> = columns
        .iter()
        .map(|value| {
            let text = display_value(value);
            bytes = bytes.saturating_add(text.len());
            vec![Span::raw(text)]
        })
        .collect();
    let rows: Vec<_> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|column| {
                    let text = row
                        .as_array()
                        .and_then(|row| row.get(column))
                        .map(display_value)
                        .unwrap_or_default();
                    bytes = bytes.saturating_add(text.len());
                    vec![Span::raw(text)]
                })
                .collect()
        })
        .collect();
    if bytes > 16_384 {
        return muted_error("table contents exceed rendering limit", width, theme);
    }
    let budget = width.saturating_sub(columns.len() * 3 + 1);
    let per_column = (budget / columns.len()).max(1);
    let data = TableData {
        col_widths: (0..columns.len())
            .map(|column| {
                headers[column][0]
                    .content
                    .width()
                    .max(
                        rows.iter()
                            .map(|row: &Vec<Vec<Span<'static>>>| row[column][0].content.width())
                            .max()
                            .unwrap_or(0),
                    )
                    .max(1)
                    .min(per_column)
            })
            .collect(),
        headers,
        rows,
        alignments: vec![pulldown_cmark_012::Alignment::Left; columns.len()],
    };
    let row_height = |row: &[Vec<Span<'static>>]| {
        row.iter()
            .map(|cell| {
                cell.iter()
                    .map(|span| span.content.chars().count())
                    .sum::<usize>()
                    .max(1)
            })
            .max()
            .unwrap_or(1)
    };
    let height = row_height(&data.headers)
        .saturating_add(data.rows.iter().map(|row| row_height(row)).sum::<usize>())
        .saturating_add(data.rows.len())
        .saturating_add(3);
    if height > 4096 || width.saturating_mul(height) > 262_144 {
        return muted_error("table buffer exceeds rendering limit", width, theme);
    }
    let mut table_theme = TableTheme::from_palette(&theme.to_palette());
    table_theme.row_style = Style::default().fg(theme.component.markdown.text);
    let mut output = GenUiRender::default();
    if let Some(title) = props.get("title").and_then(Value::as_str) {
        output.append(plain(
            title,
            width,
            Style::default()
                .fg(theme.semantic.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    output
        .lines
        .extend(table_data_to_lines(&data, &table_theme, width));
    output
}

fn display_value(value: &Value) -> String {
    sanitize(
        &value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
    )
}

fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .collect()
}

fn plain(text: &str, width: usize, style: Style) -> GenUiRender {
    let lines = sanitize(text)
        .split('\n')
        .flat_map(|line| crate::truncate::wrap_by_width(line, width.max(1)))
        .map(|line| Line::from(Span::styled(fit(&line, width), style)))
        .collect();
    GenUiRender {
        lines,
        buttons: vec![],
    }
}

fn fit(text: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut used = 0usize;
    text.graphemes(true)
        .take_while(|grapheme| {
            used = used.saturating_add(grapheme.width());
            used <= width
        })
        .collect()
}

fn muted_error(error: &str, width: usize, theme: &ThemeDefinition) -> GenUiRender {
    plain(
        &format!("gen-ui: {error}"),
        width,
        Style::default().fg(theme.semantic.text.muted),
    )
}
