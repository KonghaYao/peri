//! MCP 详情视图的四段表格渲染（工具 / 技能 / 代理 / 订阅）。
//!
//! 几何只依赖面板内容宽度：列宽、边框与单元格内边距固定；单元格按显示宽度
//! 截断并补齐。描述不换行，因此普通宽度下表格行数与宽度无关；面板窄到放不下
//! 表格时退回单行占位，行数随之收缩——鼠标命中反推的行号
//! （[`super::detail_btn_row`]）把同一宽度传给 [`section_line_count`]，两者
//! 始终同源。

use crate::i18n;
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::{
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

/// 可渲染表格的最小总宽：边框 3 + 内边距 4 + 两列各至少 1 列。
///
/// 低于此宽度时表格退回单行占位（与空数据同形态），行数随之收缩——渲染与
/// 鼠标命中共用 [`section_line_count`] 的宽度参数，不会错位。
pub(super) const TABLE_MIN_RENDER_WIDTH: usize = 9;

/// 列间边框列数（左 / 中 / 右）。
const TABLE_BORDER_COLUMNS: usize = 3;
/// 每格左右各 1 列内边距，两格共 4 列。
const TABLE_PADDING_COLUMNS: usize = 4;
/// 有数据时的表格骨架行数：顶边、表头、表头分隔、底边。
const TABLE_CHROME_LINES: usize = 4;

/// 表格行（名称/项 + 描述/值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TableRow {
    pub(super) name: String,
    pub(super) description: String,
}

/// 一段表格的完整描述（标题计数与表格行数可能不同，例如订阅）。
pub(super) struct SectionSpec<'a> {
    pub(super) label: &'a str,
    pub(super) count: usize,
    pub(super) headers: (&'a str, &'a str),
    pub(super) rows: &'a [TableRow],
    pub(super) more: Option<String>,
}

/// 一段表格的总行数：空行 + 标题 +（空占位行 | 表格）+ 可选 more 行。
///
/// 表格放不下（面板过窄）时与空数据一样只占 1 行占位。
pub(super) fn section_line_count(rows: usize, has_more: bool, panel_width: u16) -> usize {
    2 + table_line_count(rows, panel_width) + usize::from(has_more)
}

/// 表格本体行数：无数据或面板过窄时只有一行空占位（四空格 + 破折号）。
fn table_line_count(rows: usize, panel_width: u16) -> usize {
    if rows == 0 || column_widths(panel_width).is_none() {
        1
    } else {
        rows + TABLE_CHROME_LINES
    }
}

/// 按面板内容宽度推导列宽；过窄返回 None（调用方退回占位行）。
///
/// 总宽 = 内容宽 − 左缩进 2 − 滚动条预留，不再抬高到固定下限——抬高会让
/// 窄面板的表格行越过面板宽度。名称列取内宽的 2/5 并夹在 10..=30，且至少给
/// 描述列留 1 列；描述列拿剩余宽度。
fn column_widths(panel_width: u16) -> Option<(usize, usize, usize)> {
    let total =
        usize::from(panel_width).saturating_sub(2 + usize::from(super::DETAIL_RIGHT_RESERVE));
    if total < TABLE_MIN_RENDER_WIDTH {
        return None;
    }
    let inner = total - (TABLE_BORDER_COLUMNS + TABLE_PADDING_COLUMNS);
    let name = (inner * 2 / 5)
        .clamp(10, 30)
        .min(inner.saturating_sub(1).max(1));
    Some((total, name, inner - name))
}

/// 单元格清洗：控制字符替换为空格、折叠连续空白、trim。
fn sanitize_cell(value: &str) -> String {
    let replaced: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 单元格定宽渲染：按显示宽度截断（含省略号）后以空格补齐。
fn fit_cell(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let sanitized = sanitize_cell(value);
    let text = if UnicodeWidthStr::width(sanitized.as_str()) > width {
        // truncate_by_width 会追加省略号、可能超出 1 列，故先按 width−1 截断。
        truncate_by_width(&sanitized, width.saturating_sub(1))
    } else {
        sanitized
    };
    let used = UnicodeWidthStr::width(text.as_str());
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

fn border_line(
    width_name: usize,
    width_value: usize,
    left: &str,
    middle: &str,
    right: &str,
    style: Style,
) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "{left}{}{middle}{}{right}",
            "─".repeat(width_name + 2),
            "─".repeat(width_value + 2),
        ),
        style,
    ))
}

fn content_line(
    widths: (usize, usize, usize),
    left: &str,
    right: &str,
    name_style: Style,
    value_style: Style,
    border_style: Style,
    header: bool,
) -> Line<'static> {
    let (name, value) = if header {
        (
            name_style.add_modifier(Modifier::BOLD),
            value_style.add_modifier(Modifier::BOLD),
        )
    } else {
        (name_style, value_style)
    };
    let (_, name_width, value_width) = widths;
    Line::from(vec![
        Span::styled("│", border_style),
        Span::styled(format!(" {} ", fit_cell(left, name_width)), name),
        Span::styled("│", border_style),
        Span::styled(format!(" {} ", fit_cell(right, value_width)), value),
        Span::styled("│", border_style),
    ])
}

/// 渲染一段表格：顶边、表头、分隔、数据行、底边；每行显示宽度一致。
fn table_lines(
    rows: &[TableRow],
    headers: (&str, &str),
    widths: (usize, usize, usize),
    theme: &ThemeDefinition,
) -> Vec<Line<'static>> {
    let semantic = &theme.semantic;
    let border = Style::new().fg(semantic.border.dim);
    let name = Style::new().fg(semantic.text.primary);
    let value = Style::new().fg(semantic.text.dim);

    let mut lines = Vec::with_capacity(rows.len() + TABLE_CHROME_LINES);
    lines.push(border_line(widths.1, widths.2, "┌", "┬", "┐", border));
    lines.push(content_line(
        widths, headers.0, headers.1, name, value, border, true,
    ));
    lines.push(border_line(widths.1, widths.2, "├", "┼", "┤", border));
    for row in rows {
        lines.push(content_line(
            widths,
            &row.name,
            &row.description,
            name,
            value,
            border,
            false,
        ));
    }
    lines.push(border_line(widths.1, widths.2, "└", "┴", "┘", border));
    lines
}

/// 追加一段完整区块：空行、标题（label · count）、空占位或表格、可选 more 行。
pub(super) fn push_section(
    lines: &mut Vec<Line<'static>>,
    section: &SectionSpec<'_>,
    panel_width: u16,
    theme: &ThemeDefinition,
) {
    let semantic = &theme.semantic;
    lines.push(Line::from(""));
    let title = i18n::tr_args(
        "panel-mcp-table-title",
        &[
            ("label".to_string(), FluentValue::from(section.label)),
            ("count".to_string(), FluentValue::from(section.count as i64)),
        ],
    );
    lines.push(Line::from(Span::styled(
        format!("  {title}"),
        Style::new()
            .fg(semantic.text.primary)
            .add_modifier(Modifier::BOLD),
    )));
    let widths = if section.rows.is_empty() {
        None
    } else {
        column_widths(panel_width)
    };
    match widths {
        Some(widths) => {
            // 与标题/占位行保持同一缩进；宽度预算已预留这 2 列。
            for mut line in table_lines(section.rows, section.headers, widths, theme) {
                line.spans.insert(0, Span::raw("  "));
                lines.push(line);
            }
        }
        // 无数据或面板过窄：单行占位（行数与 section_line_count 同源）。
        None => lines.push(
            Line::from(format!("    {}", i18n::tr("panel-mcp-table-empty"))).fg(semantic.text.dim),
        ),
    }
    if let Some(more) = &section.more {
        lines.push(Line::from(format!("    {more}")).fg(semantic.text.dim));
    }
}
