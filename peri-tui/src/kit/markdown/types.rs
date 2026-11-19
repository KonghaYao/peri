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
    /// 保留结构和动作，宽度与主题变化时由消息渲染器重新布局。
    GenUi(GenUiSegment),
    /// Mermaid fenced code block：携带原文坐标、闭合状态与源码；
    /// `lines` 是预览不可用时的源码回退形态。
    Mermaid(MermaidSegment),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MermaidSegment {
    /// 围栏元数据（范围使用完整消息原文坐标）。
    pub fence: super::fence::MermaidFence,
    /// 与普通代码块一致的源码回退行；显示层按围栏身份取当前可信帧。
    pub lines: Vec<ratatui::text::Line<'static>>,
}

impl MermaidSegment {
    /// 组合会话、消息身份与围栏位置，得到这张图的稳定身份。
    #[must_use]
    pub fn key(
        &self,
        session_id: impl Into<String>,
        message: super::fence::MermaidMessageRef,
    ) -> super::fence::MermaidFenceKey {
        self.fence.key(session_id, message)
    }
}

#[derive(Debug, Clone)]
pub struct GenUiSegment {
    pub source: String,
    pub parsed: std::sync::Arc<peri_genui::ParseResult>,
    pub partial: bool,
    pub buttons: Vec<crate::kit::genui::GenUiButton>,
    pub(crate) root: Option<serde_json::Value>,
    pub(crate) errors: Vec<String>,
}

impl PartialEq for GenUiSegment {
    fn eq(&self, other: &Self) -> bool {
        // 解析是同一静态库上的纯函数，源文相同即可比较共享的非 PartialEq 结果。
        self.source == other.source
            && self.partial == other.partial
            && self.buttons == other.buttons
            && self.root == other.root
            && self.errors == other.errors
    }
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
