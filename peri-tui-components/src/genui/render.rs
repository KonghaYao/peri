use peri_genui::EvalContext;
use peri_theme::{bridge::ThemeDefinitionExt, theme::ThemeDefinition};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use ratatui_kit::{ComponentTheme, components::TableTheme};
use serde_json::Value;
use tui_chart::{BarChart, BarGroup, BarOrientation, Sparkline};
use unicode_width::UnicodeWidthStr;

use crate::button::{
    ButtonChrome, ButtonSpec, ButtonState, ButtonTone, button_style, button_visual,
};
use crate::charts::theme_for;
use crate::markdown::{GenUiSegment, TableData, buffer_to_lines, table_data_to_lines};

use super::{GenUiAction, GenUiButton, GenUiButtonInfo};

#[derive(Debug, Default)]
pub struct GenUiRender {
    pub lines: Vec<Line<'static>>,
    pub buttons: Vec<GenUiButtonInfo>,
}

impl GenUiRender {
    fn append(&mut self, mut other: Self) {
        // 标题自带上下空行；块与块相接时只保留一个空行，避免连续标题之间出现双空行。
        let dropped = usize::from(
            self.lines.last().is_some_and(|line| line.width() == 0)
                && other.lines.first().is_some_and(|line| line.width() == 0),
        );
        if dropped == 1 {
            other.lines.remove(0);
        }
        let offset = self.lines.len();
        for button in &mut other.buttons {
            button.line_start = button
                .line_start
                .saturating_sub(dropped)
                .saturating_add(offset);
            button.line_end = button
                .line_end
                .saturating_sub(dropped)
                .saturating_add(offset);
        }
        self.lines.extend(other.lines);
        self.buttons.extend(other.buttons);
    }
}

/// 块首/块尾的空行没有隔离对象，裁掉；同时修正按钮的行偏移。
fn trim_blank_edges(output: &mut GenUiRender) {
    let leading = output
        .lines
        .iter()
        .take_while(|line| line.width() == 0)
        .count();
    if leading > 0 {
        output.lines.drain(..leading);
        for button in &mut output.buttons {
            button.line_start = button.line_start.saturating_sub(leading);
            button.line_end = button.line_end.saturating_sub(leading);
        }
    }
    while output.lines.last().is_some_and(|line| line.width() == 0) {
        output.lines.pop();
    }
}

/// Render one OpenUI segment.
///
/// `context` carries the host's interaction state (`Set`/`Reset` actions); pass
/// `None` for a read-only preview. Rendering itself is pure.
pub fn render(
    segment: &GenUiSegment,
    width: usize,
    theme: &ThemeDefinition,
    context: Option<&EvalContext>,
) -> GenUiRender {
    if width == 0 {
        return GenUiRender::default();
    }
    let width = width.min(4096);
    let block = segment.block_id();
    let updated = context.map(|context| segment.with_context(context));
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
    trim_blank_edges(&mut output);
    for button in &mut output.buttons {
        button.block = Some(block);
        button.partial = segment.partial;
        button.parsed = matches!(button.action, GenUiAction::Set(..) | GenUiAction::Reset(..))
            .then(|| std::sync::Arc::clone(&segment.parsed));
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
            match props.get("content") {
                Some(Value::String(content)) => plain(content, width, style),
                Some(value @ (Value::Number(_) | Value::Bool(_))) => {
                    plain(&display_value(value), width, style)
                }
                Some(_) => muted_error("Text content must be a string", width, theme),
                // 流式解析可能在 content 参数生成前渲染，保留原有空行占位。
                None => plain("", width, style),
            }
        }
        "Heading" => {
            let level = props.get("level").and_then(Value::as_u64).unwrap_or(1);
            let mut output = plain(
                text("text"),
                width,
                if level <= 2 {
                    heading
                } else {
                    body.add_modifier(Modifier::BOLD)
                },
            );
            // 标题与相邻内容之间各留一个空行；块首/块尾的空行在 `render` 末尾裁掉，
            // 相接块之间的重复空行由 `append` 合并。
            output.lines.insert(0, Line::default());
            output.lines.push(Line::default());
            output
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
        "BarChart" => bar_chart(props, width, theme),
        "Sparkline" => sparkline(props, width, theme),
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
    let label = fit(
        &sanitize(&descriptor.label).replace('\n', " "),
        width.saturating_sub(2),
    );
    let tone = if props.get("variant").and_then(Value::as_str) == Some("primary") {
        ButtonTone::Primary
    } else {
        ButtonTone::Secondary
    };
    let visual = button_visual(
        ButtonSpec {
            label: &label,
            chrome: ButtonChrome::Filled,
            tone,
            state: ButtonState::Normal,
        },
        theme,
    );
    GenUiRender {
        lines: vec![Line::from(Span::styled(visual.text, visual.style))],
        buttons: vec![GenUiButtonInfo {
            line_start: 0,
            line_end: 1,
            x_start: 0,
            // 命中宽度沿用标签宽度加两列留白的既有契约。
            x_end: label.width().saturating_add(2).min(u16::MAX as usize) as u16,
            block: None,
            parsed: None,
            partial: false,
            action,
            label: descriptor.label,
        }],
    }
}

/// 从 Normal 按钮的前景色恢复 tone，无需在按钮布局数据中保存样式。
pub fn pressed_button_style(normal_fg: Color, theme: &ThemeDefinition) -> Style {
    let tone = if normal_fg == theme.semantic.accent {
        ButtonTone::Primary
    } else {
        ButtonTone::Secondary
    };
    button_style(ButtonChrome::Filled, tone, ButtonState::Pressed, theme)
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

fn bar_chart(
    props: &serde_json::Map<String, Value>,
    width: usize,
    theme: &ThemeDefinition,
) -> GenUiRender {
    let Some(items) = props.get("items").and_then(Value::as_array) else {
        return muted_error("bar chart needs an items array", width, theme);
    };
    if items.is_empty() {
        return muted_error("bar chart needs at least one item", width, theme);
    }
    if items.len() > 64 {
        return muted_error("bar chart supports at most 64 items", width, theme);
    }
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let label = item
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(raw) = item.get("value") else {
            return muted_error("bar chart items need a value", width, theme);
        };
        let Some(value) = raw.as_f64() else {
            return muted_error("bar chart values must be numbers", width, theme);
        };
        if !value.is_finite() || value < 0.0 {
            return muted_error(
                "bar chart values must be non-negative numbers",
                width,
                theme,
            );
        }
        // tui-chart 的 y 轴 gutter 上限 16 列；先按显示宽度截断，避免 CJK 标签
        // 按字符截断后溢出绘图区。
        entries.push((
            fit(&sanitize(label).replace('\n', " "), 16),
            value,
            display_value(raw),
        ));
    }
    let max = match props.get("max") {
        None => entries
            .iter()
            .map(|(_, value, _)| *value)
            .fold(0.0, f64::max)
            .max(1.0),
        Some(raw) => match raw.as_f64() {
            Some(max) if max.is_finite() && max > 0.0 => max,
            _ => return muted_error("bar chart max must be a positive number", width, theme),
        },
    };
    let gutter = entries
        .iter()
        .map(|(label, _, _)| label.width())
        .max()
        .unwrap_or(0);
    // tui-chart 的 outside 数值标签在条形贴边或值为 0 时不绘制（`total > 0`
    // 与右边界检查），这里保留独立数值列：绘图区让出最宽数值 + 1 列间隔，
    // 数值由本函数逐行补齐，保证每个条目都可见。
    let value_width = entries
        .iter()
        .map(|(_, _, text)| text.width())
        .max()
        .unwrap_or(1)
        .max(1);
    let chart_width = width.saturating_sub(value_width + 1);
    if chart_width < gutter + 3 {
        return muted_error("bar chart needs more width", width, theme);
    }
    // 1 行 x 轴 + 1 行刻度 + 至少 2 行绘图区；该高度下每条 bar 正好 1 行。
    let height = (entries.len() + 2).max(4);
    let area = Rect::new(0, 0, width as u16, height as u16);
    let chart_area = Rect::new(0, 0, chart_width as u16, height as u16);
    let mut buffer = Buffer::empty(area);
    let groups = entries
        .iter()
        .map(|(label, value, _)| BarGroup::new(label.clone(), [*value]))
        .collect::<Vec<_>>();
    BarChart::new(groups)
        .orientation(BarOrientation::Horizontal)
        .show_values(false)
        .max(max)
        .theme(theme_for(theme))
        .render(chart_area, &mut buffer);
    let value_style = Style::default().fg(theme.component.markdown.text);
    for (row, (_, _, text)) in entries.iter().enumerate() {
        // 数值列右对齐，与旧渲染器的补空格行为一致。
        let x = width.saturating_sub(text.width());
        buffer.set_string(x as u16, row as u16, text, value_style);
    }
    if (0..chart_width).all(|x| (0..height).all(|y| buffer[(x as u16, y as u16)].symbol() == " ")) {
        return muted_error("bar chart needs more width", width, theme);
    }
    let heading = Style::default()
        .fg(theme.semantic.accent)
        .add_modifier(Modifier::BOLD);
    let mut output = GenUiRender::default();
    if let Some(title) = props.get("title").and_then(Value::as_str)
        && !title.is_empty()
    {
        output.append(plain(title, width, heading));
    }
    output.lines.extend(buffer_to_lines(&buffer, area));
    output
}

fn sparkline(
    props: &serde_json::Map<String, Value>,
    width: usize,
    theme: &ThemeDefinition,
) -> GenUiRender {
    let Some(values) = props.get("values").and_then(Value::as_array) else {
        return muted_error("sparkline needs a values array", width, theme);
    };
    if values.is_empty() {
        return muted_error("sparkline needs at least one value", width, theme);
    }
    if values.len() > 4096 {
        return muted_error("sparkline supports at most 4096 values", width, theme);
    }
    let mut numbers = Vec::with_capacity(values.len());
    for value in values {
        let Some(number) = value.as_f64() else {
            return muted_error("sparkline values must be numbers", width, theme);
        };
        if !number.is_finite() {
            return muted_error("sparkline values must be finite numbers", width, theme);
        }
        numbers.push(number);
    }
    let label = props
        .get("label")
        .and_then(Value::as_str)
        .map(|label| sanitize(label).replace('\n', " "))
        .unwrap_or_default();
    let prefix = if label.is_empty() {
        String::new()
    } else {
        format!("{label}  ")
    };
    let last = values.last().map(display_value).unwrap_or_default();
    let suffix = format!("  {last}");
    let available = width.saturating_sub(prefix.width() + suffix.width());
    if available < 2 {
        return muted_error("sparkline needs more width", width, theme);
    }
    let visible = &numbers[numbers.len().saturating_sub(available)..];
    // tui-chart 每个数据点占一列；按点数给宽度，末值才会紧跟图形而不是被推到行尾。
    let area = Rect::new(0, 0, visible.len() as u16, 1);
    let mut buffer = Buffer::empty(area);
    Sparkline::new(visible.iter().copied())
        .theme(theme_for(theme))
        .render(area, &mut buffer);
    let mut spans = vec![Span::styled(
        prefix,
        Style::default().fg(theme.component.markdown.text),
    )];
    if let Some(line) = buffer_to_lines(&buffer, area).into_iter().next() {
        spans.extend(line.spans);
    }
    spans.push(Span::styled(
        suffix,
        Style::default().fg(theme.semantic.text.muted),
    ));
    GenUiRender {
        lines: vec![Line::from(spans)],
        buttons: vec![],
    }
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
        .flat_map(|line| crate::text::wrap_by_width(line, width.max(1)))
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
