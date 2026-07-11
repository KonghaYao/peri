use pulldown_cmark::Alignment;
use ratatui::text::Span;

/// Markdown 解析输出的一个段落。
#[derive(Debug, Clone)]
pub enum MarkdownSegment {
    /// 纯文本行（段落、标题、代码块、列表、分隔线等）。
    Text(Vec<ratatui::text::Line<'static>>),
    /// 表格数据，由 `table_data_to_lines` (ratatui-kit 风格 unicode 网格线) 渲染。
    Table(TableData),
}

/// 表格结构化数据。
#[derive(Debug, Clone)]
pub struct TableData {
    pub headers: Vec<Vec<Span<'static>>>,
    pub rows: Vec<Vec<Vec<Span<'static>>>>,
    pub alignments: Vec<Alignment>,
    /// 每列内容宽度（已做等比例缩放适配 max_width）
    pub col_widths: Vec<usize>,
}
