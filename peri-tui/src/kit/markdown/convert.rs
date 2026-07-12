use ratatui::text::{Line, Span};
use ratatui_kit_markdown::{MarkdownTheme, ParsedBlock};

use super::code_block::code_block_lines;
use super::heading::heading_line;
use super::list::list_item_line;
use super::span_style::apply_span_styles;
use super::table::compute_table_col_widths;
use super::types::{MarkdownSegment, TableData};

// ── 块级转换 ────────────────────────────────────────────────────────

/// 将 ratatui-kit-markdown 的 ParsedBlock 列表转换为 MarkdownSegment 序列。
///
/// 间距规则（统一）：
/// - 每个块级元素前加 **恰好一行**空行，除非：
///   (a) 是第一个有内容的块
///   (b) 是连续的列表项（列表项之间无空行）
/// - parser 生成的空 `Paragraph` 是列表分隔哨兵，跳过（间距由本函数统一管理）
pub(crate) fn convert_to_segments(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    max_width: usize,
) -> Vec<MarkdownSegment> {
    let mut segments: Vec<MarkdownSegment> = Vec::new();
    let mut current_text: Vec<Line<'static>> = Vec::new();
    let mut prev_was_list_item = false;

    for block in blocks {
        // 跳过 parser 生成的空 Paragraph（列表前后的哨兵）
        if matches!(block, ParsedBlock::Paragraph(lines) if lines.is_empty()) {
            prev_was_list_item = false;
            continue;
        }

        let is_list_item = matches!(block, ParsedBlock::ListItem(_));

        // 分隔：非首块 + 非连续列表项 → 确保恰好一行空行
        if !(current_text.is_empty()
            || current_text.last().is_some_and(|l| l.spans.is_empty())
            || is_list_item && prev_was_list_item)
        {
            current_text.push(Line::default());
        }

        match block {
            ParsedBlock::Heading(level, line) => {
                current_text.push(heading_line(level, line, theme));
            }
            ParsedBlock::Paragraph(para_lines) => {
                for line in para_lines {
                    current_text.push(style_line(line, theme));
                }
            }
            ParsedBlock::CodeBlock(lang, code_lines) => {
                current_text.extend(code_block_lines(lang, code_lines, theme));
            }
            ParsedBlock::ListItem(item) => {
                current_text.push(list_item_line(item, theme));
            }
            ParsedBlock::Rule => {
                let rule_char = "─".repeat(max_width.min(80));
                let rule_span = Span::styled(rule_char, theme.rule_style);
                current_text.push(Line::from(rule_span));
            }
            ParsedBlock::Table(headers, rows, alignments) => {
                // 表格前：冲刷已有文本为独立段
                trim_trailing_blanks(&mut current_text);
                if !current_text.is_empty() {
                    segments.push(MarkdownSegment::Text(std::mem::take(&mut current_text)));
                }
                let col_widths =
                    compute_table_col_widths(headers, rows, alignments.len(), max_width);
                segments.push(MarkdownSegment::Table(TableData {
                    headers: headers.clone(),
                    rows: rows.clone(),
                    alignments: alignments.clone(),
                    col_widths,
                }));
            }
        }

        prev_was_list_item = is_list_item;
    }

    // 冲刷剩余文本
    trim_trailing_blanks(&mut current_text);
    if !current_text.is_empty() {
        segments.push(MarkdownSegment::Text(current_text));
    }
    segments
}

/// 裁剪尾部空行。
fn trim_trailing_blanks(text: &mut Vec<Line<'static>>) {
    while text.last().is_some_and(|l| l.spans.is_empty()) {
        text.pop();
    }
}

/// 通用段落行渲染。
fn style_line(line: &Line<'static>, theme: &MarkdownTheme) -> Line<'static> {
    Line::from(apply_span_styles(&line.spans, theme, None))
}
