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
pub(crate) fn convert_to_segments(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    max_width: usize,
) -> Vec<MarkdownSegment> {
    let mut segments: Vec<MarkdownSegment> = Vec::new();
    let mut current_text: Vec<Line<'static>> = Vec::new();
    let mut prev_added_trailing = false;
    let mut prev_was_major = false;
    let mut prev_was_real_para = false;

    let flush_text = |text: &mut Vec<Line<'static>>, segs: &mut Vec<MarkdownSegment>| {
        // 裁剪尾部空行
        while text.last().is_some_and(|l| l.spans.is_empty()) {
            text.pop();
        }
        if !text.is_empty() {
            segs.push(MarkdownSegment::Text(std::mem::take(text)));
        }
    };

    for block in blocks {
        let is_major = matches!(
            block,
            ParsedBlock::Heading(..)
                | ParsedBlock::CodeBlock(..)
                | ParsedBlock::Table(..)
                | ParsedBlock::Rule
        );

        if !prev_added_trailing && prev_was_major && is_major {
            current_text.push(Line::default());
        }
        prev_added_trailing = false;

        let is_real_para = matches!(block, ParsedBlock::Paragraph(ps) if !ps.is_empty());
        if is_real_para && prev_was_real_para {
            current_text.push(Line::default());
        }

        match block {
            ParsedBlock::Heading(level, line) => {
                current_text.push(heading_line(level, line, theme));
                current_text.push(Line::default());
                prev_added_trailing = true;
            }
            ParsedBlock::Paragraph(para_lines) => {
                if para_lines.is_empty() {
                    current_text.push(Line::default());
                } else {
                    for line in para_lines {
                        current_text.push(style_line(line, theme));
                    }
                }
            }
            ParsedBlock::CodeBlock(lang, code_lines) => {
                current_text.push(Line::default());
                current_text.extend(code_block_lines(lang, code_lines, theme));
                current_text.push(Line::default());
                prev_added_trailing = true;
            }
            ParsedBlock::ListItem(item) => {
                current_text.push(list_item_line(item, theme));
            }
            ParsedBlock::Table(headers, rows, alignments) => {
                flush_text(&mut current_text, &mut segments);
                let col_widths =
                    compute_table_col_widths(headers, rows, alignments.len(), max_width);
                segments.push(MarkdownSegment::Table(TableData {
                    headers: headers.clone(),
                    rows: rows.clone(),
                    alignments: alignments.clone(),
                    col_widths,
                }));
            }
            ParsedBlock::Rule => {
                let rule_char = "─".repeat(max_width.min(80));
                let rule_span = Span::styled(rule_char, theme.rule_style);
                current_text.push(Line::from(rule_span));
                current_text.push(Line::default());
                prev_added_trailing = true;
            }
        }

        prev_was_major = is_major;
        prev_was_real_para = is_real_para;
    }

    flush_text(&mut current_text, &mut segments);
    segments
}

/// 通用段落行渲染。
fn style_line(line: &Line<'static>, theme: &MarkdownTheme) -> Line<'static> {
    Line::from(apply_span_styles(&line.spans, theme, None))
}
