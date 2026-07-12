//! Markdown 解析（kit 路径专用）。
//!
//! 底层委托给 `ratatui_kit_markdown::parse_markdown`（公开 API），
//! 自行实现 `ParsedBlock` → `Line<'static>` 转换以适配 RENDER_CACHE 管线。
//! `ratatui_kit_markdown` 的 `RenderRow` / `render_rows_with_theme` 为
//! `pub(crate)`，外部不可用——此处复刻了 `style_spans` / `semantic_style`
//! 及块间距逻辑。
//!
//! 子模块组织：
//! - `types`：MarkdownSegment, TableData
//! - `span_style`：apply_span_styles, span_semantic_style
//! - `heading`：heading_line（不渲染 # 前缀）
//! - `list`：list_item_line
//! - `code_block`：highlight_code_block, code_block_lines, syntect 单例
//! - `table`：compute_table_col_widths, table_data_to_lines (ratatui-kit 风格渲染)
//! - `convert`：convert_to_segments（块级分发）

mod code_block;
mod convert;
mod heading;
mod list;
mod span_style;
mod table;
pub mod types;

use ratatui_kit::{ComponentTheme, prelude::Palette};
use ratatui_kit_markdown::{MarkdownTheme, parse_markdown as rk_parse};

pub use table::table_data_to_lines;
pub use types::{MarkdownSegment, TableData};

// ── 公开 API ───────────────────────────────────────────────────────

/// 解析 markdown 为段落序列，表格作为独立 `Table` 段，不放 `Vec<Line>` 里。
pub fn parse_markdown(input: &str, max_width: usize, palette: Palette) -> Vec<MarkdownSegment> {
    if input.is_empty() {
        return vec![];
    }
    let parsed = rk_parse(input);
    let theme = MarkdownTheme::from_palette(&palette);
    convert::convert_to_segments(&parsed.blocks, &theme, max_width)
}

// ── 测试 ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    /// 测试辅助：将 parse_markdown 返回的段落展平为 Line 列表。
    fn flatten(segments: &[MarkdownSegment]) -> Vec<ratatui::text::Line<'static>> {
        segments
            .iter()
            .flat_map(|s| match s {
                MarkdownSegment::Text(lines) => lines.clone(),
                MarkdownSegment::Table(_) => vec![],
            })
            .collect()
    }

    #[test]
    fn test_empty_input() {
        let result = flatten(&parse_markdown("", 80, Palette::default()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_heading() {
        let result = flatten(&parse_markdown("# Hello", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        let line = &result[0];
        // 不渲染 # 前缀，标题文本当普通段落
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Hello");
    }

    #[test]
    fn test_paragraph() {
        let result = flatten(&parse_markdown("hello world", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].spans[0].content, "hello world");
    }

    #[test]
    fn test_adjacent_paragraphs() {
        let result = flatten(&parse_markdown("a\n\nb", 80, Palette::default()));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].spans[0].content, "a");
        assert!(result[1].spans.is_empty());
        assert_eq!(result[2].spans[0].content, "b");
    }

    #[test]
    fn test_inline_code() {
        let result = flatten(&parse_markdown("use `code` here", 80, Palette::default()));
        let line = &result[0];
        // 不渲染 ` 符号的特殊样式，行内代码当普通文本
        let code_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "`code`")
            .expect("inline code span should still contain backtick markers");
        assert_eq!(
            code_span.style.fg, None,
            "inline code should not have special fg"
        );
    }

    #[test]
    fn test_unordered_list() {
        let result = flatten(&parse_markdown(
            "- item 1\n- item 2",
            80,
            Palette::default(),
        ));
        let non_empty: Vec<_> = result.iter().filter(|l| !l.spans.is_empty()).collect();
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
        let result = flatten(&parse_markdown(
            "```rust\nlet x = 1;\n```",
            80,
            Palette::default(),
        ));
        assert!(result.len() >= 2);
    }

    #[test]
    fn test_rule() {
        let result = flatten(&parse_markdown("---", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        let content: String = result[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains('─'));
    }

    #[test]
    fn test_bold_text() {
        let result = flatten(&parse_markdown("**bold**", 80, Palette::default()));
        let line = &result[0];
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "bold text should have BOLD modifier"
        );
    }
}
