//! Markdown 解析（kit 路径专用）。
//!
//! 底层委托给 `ratatui_kit_markdown::parse_markdown`（公开 API），
//! 自行实现 `ParsedBlock` → `Line<'static>` 转换以适配 RENDER_CACHE 管线。
//! `ratatui_kit_markdown` 的 `RenderRow` / `render_rows_with_theme` 为
//! `pub(crate)`，外部不可用——此处复刻了 `style_spans` / `semantic_style`
//! 及块间距逻辑。

use std::sync::LazyLock;

use pulldown_cmark::{Alignment, HeadingLevel};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span, Text},
};
use ratatui_kit::ComponentTheme;
use ratatui_kit_markdown::{ListItemData, MarkdownTheme, ParsedBlock, parse_markdown as rk_parse};
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};
use unicode_width::UnicodeWidthStr;

use crate::kit::theme::markdown_palette::peri_markdown_palette;

// ── syntect 全局单例 ───────────────────────────────────────────────

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// ── 公开 API ───────────────────────────────────────────────────────

/// 解析 markdown 文本为 ratatui Text。
pub fn parse_markdown(input: &str, max_width: usize) -> Text<'static> {
    if input.is_empty() {
        return Text::default();
    }

    let parsed = rk_parse(input);
    let palette = peri_markdown_palette();
    let theme = MarkdownTheme::from_palette(&palette);
    let mut lines = convert_blocks(&parsed.blocks, &theme, max_width);

    // 裁剪尾部空行
    while lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }

    Text::from(lines)
}

/// 解析 markdown 文本为 ratatui Text（默认宽度 80）。
pub fn parse_markdown_default(input: &str) -> Text<'static> {
    parse_markdown(input, 80)
}

// ── 块级转换 ────────────────────────────────────────────────────────

fn convert_blocks(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut prev_added_trailing = false;
    let mut prev_was_major = false;
    let mut prev_was_real_para = false;

    for block in blocks {
        let is_major = matches!(
            block,
            ParsedBlock::Heading(..)
                | ParsedBlock::CodeBlock(..)
                | ParsedBlock::Table(..)
                | ParsedBlock::Rule
        );

        // 相邻 major 块之间补空行（上一个 major 无 trailing 空行时）
        if !prev_added_trailing && prev_was_major && is_major {
            lines.push(Line::default());
        }
        prev_added_trailing = false;

        // 连续两个真实段落之间补空行
        let is_real_para = matches!(block, ParsedBlock::Paragraph(ps) if !ps.is_empty());
        if is_real_para && prev_was_real_para {
            lines.push(Line::default());
        }

        match block {
            ParsedBlock::Heading(level, line) => {
                lines.push(heading_line(level, line, theme));
                lines.push(Line::default());
                prev_added_trailing = true;
            }
            ParsedBlock::Paragraph(para_lines) => {
                if para_lines.is_empty() {
                    lines.push(Line::default());
                } else {
                    for line in para_lines {
                        lines.push(style_line(line, theme));
                    }
                }
            }
            ParsedBlock::CodeBlock(lang, code_lines) => {
                lines.push(Line::default());
                lines.extend(code_block_lines(lang, code_lines, theme));
                lines.push(Line::default());
                prev_added_trailing = true;
            }
            ParsedBlock::ListItem(item) => {
                lines.push(list_item_line(item, theme));
            }
            ParsedBlock::Table(headers, rows, alignments) => {
                lines.extend(table_lines(
                    headers,
                    rows,
                    alignments.as_slice(),
                    theme,
                    max_width,
                ));
            }
            ParsedBlock::Rule => {
                let rule_char = "─".repeat(max_width.min(80));
                let rule_span = Span::styled(rule_char, theme.rule_style);
                lines.push(Line::from(rule_span));
            }
        }

        prev_was_major = is_major;
        prev_was_real_para = is_real_para;
    }

    lines
}

// ── 各变体渲染 ──────────────────────────────────────────────────────

fn heading_level_num(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_line(
    level: &HeadingLevel,
    line: &Line<'static>,
    theme: &MarkdownTheme,
) -> Line<'static> {
    let level_num = heading_level_num(*level);
    let prefix = "#".repeat(level_num);
    let mut spans = vec![
        Span::styled(prefix, theme.heading_marker_style),
        Span::raw(" "),
    ];
    spans.extend(apply_span_styles(
        &line.spans,
        theme,
        Some(theme.heading_style),
    ));
    Line::from(spans)
}

fn list_item_line(item: &ListItemData, theme: &MarkdownTheme) -> Line<'static> {
    let indent = "  ".repeat(item.depth as usize);
    let prefix = if item.ordered {
        format!("{}{}. ", indent, item.number.unwrap_or(1))
    } else {
        format!("{indent}• ")
    };
    let mut spans = vec![Span::styled(prefix, theme.list_marker_style)];
    spans.extend(apply_span_styles(&item.spans, theme, None));
    Line::from(spans)
}

fn style_line(line: &Line<'static>, theme: &MarkdownTheme) -> Line<'static> {
    Line::from(apply_span_styles(&line.spans, theme, None))
}

// ── Span 样式处理（复刻 ratatui-kit-markdown 的 style_spans）───────

fn apply_span_styles(
    spans: &[Span<'static>],
    theme: &MarkdownTheme,
    base_style: Option<Style>,
) -> Vec<Span<'static>> {
    spans
        .iter()
        .map(|span| {
            let content = span.content.clone();
            let mut carried_style = span.style;
            // 剥离 parser 内部的 LINK_URL_MARKER 哨兵（Modifier::REVERSED），
            // 否则终端实际渲染会反转前景/背景色。
            carried_style.add_modifier.remove(Modifier::REVERSED);
            let style = span_semantic_style(span, theme)
                .or(base_style)
                .unwrap_or_default()
                .patch(carried_style);
            Span::styled(content, style)
        })
        .collect()
}

fn span_semantic_style(span: &Span<'static>, theme: &MarkdownTheme) -> Option<Style> {
    let text = span.content.as_ref();
    if span.style.add_modifier.contains(Modifier::REVERSED) {
        // LINK_URL_MARKER 哨兵 → link_url_style
        Some(theme.link_url_style)
    } else if text.len() >= 2 && text.starts_with('`') && text.ends_with('`') {
        Some(theme.inline_code_style)
    } else if span.style.add_modifier.contains(Modifier::UNDERLINED) {
        Some(theme.link_style)
    } else {
        None
    }
}

// ── 代码块高亮 ──────────────────────────────────────────────────────

fn highlight_code_block(lang: &str, raw_lines: &[String]) -> Option<Vec<Line<'static>>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss.find_syntax_by_token(lang)?;
    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    let mut result = Vec::with_capacity(raw_lines.len());
    for line_text in raw_lines {
        let ranges = highlighter.highlight_line(line_text, ss).ok()?;
        let spans: Vec<Span<'static>> = ranges
            .iter()
            .map(|(style, text)| {
                let color = ratatui::style::Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                Span::styled(text.to_string(), Style::default().fg(color))
            })
            .collect();
        result.push(Line::from(spans));
    }
    Some(result)
}

fn code_block_lines(lang: &str, raw_lines: &[String], theme: &MarkdownTheme) -> Vec<Line<'static>> {
    let lang_clean = lang.trim();
    let highlighted = highlight_code_block(lang_clean, raw_lines);

    if raw_lines.len() == 1 {
        // 单行代码块：inline code style
        if let Some(hl_lines) = highlighted {
            return hl_lines;
        }
        return vec![Line::from(Span::styled(
            raw_lines[0].clone(),
            theme.inline_code_style,
        ))];
    }

    // 多行代码块：每行加 `│ ` 前缀
    let prefix_style = theme.rule_style;
    let prefix = Span::styled("│ ", prefix_style);

    if let Some(hl_lines) = highlighted {
        hl_lines
            .into_iter()
            .map(|line| {
                let mut spans = vec![prefix.clone()];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect()
    } else {
        raw_lines
            .iter()
            .map(|raw| {
                Line::from(vec![
                    prefix.clone(),
                    Span::styled(raw.clone(), Style::default()),
                ])
            })
            .collect()
    }
}

// ── 表格渲染 ────────────────────────────────────────────────────────

fn table_lines(
    headers: &[Vec<Span<'static>>],
    rows: &[Vec<Vec<Span<'static>>>],
    alignments: &[Alignment],
    theme: &MarkdownTheme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let col_count = headers
        .len()
        .max(rows.first().map(|r| r.len()).unwrap_or(0));
    if col_count == 0 {
        return vec![Line::default()];
    }

    // 计算每列显示宽度（unicode-width）
    let mut col_widths = vec![0usize; col_count];
    for (i, cell) in headers.iter().enumerate() {
        col_widths[i] = col_widths[i].max(span_width(cell));
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                col_widths[i] = col_widths[i].max(span_width(cell));
            }
        }
    }
    for w in &mut col_widths {
        *w = (*w).max(3); // 最小列宽
    }

    // 可用宽度校验：边框 = 1（起始│）+ 3*col_count（每列右边框 │）+ (col_count-1)（列间无额外）
    // 实际公式：每列需要 width+2（左右各 1 空白填充）
    let total_border = 1 + 3 * col_count;
    let content_width: usize = col_widths.iter().sum::<usize>();
    let needed_width = total_border + content_width;
    if needed_width > max_width {
        // 等比例缩放到可用空间
        let available = max_width.saturating_sub(total_border);
        if available > 0 {
            let total: usize = col_widths.iter().sum();
            if total > 0 {
                let mut allocated: usize = 0;
                for (i, w) in col_widths.iter_mut().enumerate() {
                    if i == col_count - 1 {
                        *w = available.saturating_sub(allocated).max(2);
                    } else {
                        *w = (*w * available / total).max(2);
                        allocated += *w;
                    }
                }
            }
        } else {
            return vec![Line::default()];
        }
    }

    let border_color = theme
        .table_border_style
        .fg
        .unwrap_or(ratatui::style::Color::Gray);
    let border_style = Style::default().fg(border_color);
    let text_style = Style::default();

    let mut out: Vec<Line<'static>> = Vec::new();

    // 顶部边框 ┌───┬───┐
    out.push(table_border_line("┌", "┬", "┐", &col_widths, border_style));

    // 表头
    out.push(table_data_line(
        headers,
        alignments,
        &col_widths,
        border_style,
        text_style,
    ));

    if !rows.is_empty() {
        // 表头分隔线 ├───┼───┤
        out.push(table_border_line("├", "┼", "┤", &col_widths, border_style));
    }

    // 数据行
    for row in rows {
        out.push(table_data_line(
            row,
            alignments,
            &col_widths,
            border_style,
            text_style,
        ));
    }

    // 底部边框 └───┴───┘
    out.push(table_border_line("└", "┴", "┘", &col_widths, border_style));

    out
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn table_border_line(
    left: &str,
    cross: &str,
    right: &str,
    col_widths: &[usize],
    style: Style,
) -> Line<'static> {
    let mut parts = vec![Span::styled(left.to_string(), style)];
    for (i, w) in col_widths.iter().enumerate() {
        parts.push(Span::styled("─".repeat(*w), style));
        if i < col_widths.len() - 1 {
            parts.push(Span::styled(cross.to_string(), style));
        }
    }
    parts.push(Span::styled(right.to_string(), style));
    Line::from(parts)
}

fn table_data_line(
    cells: &[Vec<Span<'static>>],
    alignments: &[Alignment],
    col_widths: &[usize],
    border_style: Style,
    text_style: Style,
) -> Line<'static> {
    let mut parts = vec![Span::styled("│", border_style)];
    for (i, w) in col_widths.iter().enumerate() {
        parts.push(Span::styled(" ", text_style));
        let cell_spans = cells.get(i).map(|c| c.as_slice()).unwrap_or(&[]);
        let cell_text: String = cell_spans.iter().map(|s| s.content.as_ref()).collect();
        let cell_width = span_width(cell_spans);

        let alignment = alignments.get(i);
        let padded = match alignment {
            Some(Alignment::Center) => {
                let left_pad = (w - cell_width) / 2;
                let right_pad = w - cell_width - left_pad;
                format!(
                    "{}{}{}",
                    " ".repeat(left_pad),
                    cell_text,
                    " ".repeat(right_pad)
                )
            }
            Some(Alignment::Right) => {
                format!("{:>width$}", cell_text, width = w)
            }
            _ => {
                format!("{:<width$}", cell_text, width = w)
            }
        };

        // 对 padding 后的文本，前半部分 padding 用 text_style，实际内容保留原始 span 样式
        // 简化处理：整个单元格用 text_style，内容部分尝试保留第一个 span 的样式
        let content_style = cell_spans
            .first()
            .map(|s| text_style.patch(s.style))
            .unwrap_or(text_style);
        parts.push(Span::styled(padded, content_style));
        parts.push(Span::styled(" ", text_style));
        parts.push(Span::styled("│", border_style));
    }
    Line::from(parts)
}

// ── 测试 ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input() {
        let result = parse_markdown("", 80);
        assert!(result.lines.is_empty());
    }

    #[test]
    fn test_heading() {
        let result = parse_markdown("# Hello", 80);
        assert_eq!(result.lines.len(), 1);
        let line = &result.lines[0];
        // "#" + " " + "Hello"
        assert_eq!(line.spans[0].content, "#");
        assert_eq!(line.spans[1].content, " ");
        assert_eq!(line.spans[2].content, "Hello");
    }

    #[test]
    fn test_paragraph() {
        let result = parse_markdown("hello world", 80);
        assert_eq!(result.lines.len(), 1);
        assert_eq!(result.lines[0].spans[0].content, "hello world");
    }

    #[test]
    fn test_adjacent_paragraphs() {
        let result = parse_markdown("a\n\nb", 80);
        assert_eq!(result.lines.len(), 3);
        assert_eq!(result.lines[0].spans[0].content, "a");
        assert!(result.lines[1].spans.is_empty());
        assert_eq!(result.lines[2].spans[0].content, "b");
    }

    #[test]
    fn test_inline_code() {
        let result = parse_markdown("use `code` here", 80);
        let line = &result.lines[0];
        let code_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "`code`")
            .expect("inline code span");
        // inline code style 应已生效（非默认样式）
        assert!(
            code_span.style.fg.is_some() || code_span.style.bg.is_some(),
            "inline code should have non-default style"
        );
    }

    #[test]
    fn test_unordered_list() {
        let result = parse_markdown("- item 1\n- item 2", 80);
        // ratatui-kit-markdown parser 可能产出末尾空行，只校验非空行
        let non_empty: Vec<_> = result
            .lines
            .iter()
            .filter(|l| !l.spans.is_empty())
            .collect();
        assert_eq!(non_empty.len(), 2, "expected 2 non-empty list item lines");
        assert!(
            non_empty[0]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "• ")
        );
        assert!(
            non_empty[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "• ")
        );
    }

    #[test]
    fn test_code_block() {
        let result = parse_markdown("```rust\nlet x = 1;\n```", 80);
        // 空行 + code 行 + 空行
        assert!(result.lines.len() >= 2);
    }

    #[test]
    fn test_rule() {
        let result = parse_markdown("---", 80);
        assert_eq!(result.lines.len(), 1);
        // horizontal rule should be a line of dashes
        let content: String = result.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(content.contains('─'));
    }

    #[test]
    fn test_bold_text() {
        let result = parse_markdown("**bold**", 80);
        let line = &result.lines[0];
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "bold text should have BOLD modifier"
        );
    }
}
