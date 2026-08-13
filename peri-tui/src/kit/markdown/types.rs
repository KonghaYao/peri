use pulldown_cmark_012::Alignment;
use ratatui::text::Span;

/// Markdown 解析输出的一个段落。
#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownSegment {
    /// 纯文本行（段落、标题、代码块、列表、分隔线等）。
    Text(Vec<ratatui::text::Line<'static>>),
    /// 表格数据，由 `table_data_to_lines` (ratatui-kit 风格 unicode 网格线) 渲染。
    Table(TableData),
    /// 图片（类型化语义——P2 消息流内联像素的升级接口，§6.1 Q2）。
    /// P0 渲染为文本降级行（§8.1 R4 三式），像素渲染由 T7 overlay 承接。
    Image(ImageSegment),
}

/// 一张图片的渲染期语义与 P0 文本降级行。
///
/// 由 convert 阶段从 T2 扫描器 side table（`ImageInfo`）构建；`lines` 已在
/// convert 阶段经 `wrap_styled_line` 折行（超宽 url 不丢内容，TUI-TEXT-001）。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageSegment {
    /// 展示字段（已过 T5 控制字符过滤 + 长度截断：alt ≤ 64 字符）。
    pub alt: String,
    /// 展示字段（已过 T5 控制字符过滤 + 长度截断：url 显示 ≤ 200 字符）。
    pub url: String,
    /// 图片 title（T5 过滤 + 截断后；无 title 为 None）。
    pub title: Option<String>,
    /// http/https scheme → true（展示层选 `[Remote image: …]` 标签，§8.1 R4）。
    pub is_remote: bool,
    /// 独占段落（段落除图片 token 外无其他文本）→ 段级间距规则；行内混排 → false。
    pub standalone: bool,
    /// sanitized 文本坐标系（side table 原始区间，P2 诊断/联动用）。
    pub byte_start: usize,
    pub byte_end: usize,
    /// convert 阶段构建的降级渲染行（含样式 + wrap_styled_line 折行）。
    /// 标签 span 用正文 base_style；url span 用 theme.link_style（下划线，链接语义）。
    pub lines: Vec<ratatui::text::Line<'static>>,
}

/// 表格结构化数据。
#[derive(Debug, Clone, PartialEq)]
pub struct TableData {
    pub headers: Vec<Vec<Span<'static>>>,
    pub rows: Vec<Vec<Vec<Span<'static>>>>,
    pub alignments: Vec<Alignment>,
    /// 每列内容宽度（已做等比例缩放适配 max_width）
    pub col_widths: Vec<usize>,
}
