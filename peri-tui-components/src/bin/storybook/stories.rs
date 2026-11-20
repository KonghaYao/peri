//! Story fixtures, rendering and mouse semantics for the TUI storybook.
//!
//! Interaction mirrors the host TUI:
//! - TextArea: left-click places the caret using wrapped visual row/display column.
//! - List: left-click is "click as enter" (selection); header/footer rows are inert and
//!   the scrollbar column is excluded; the wheel scrolls the viewport by `WHEEL_ROWS`
//!   (the host's `SCROLL_LINES`) without moving the selection.
//! - Scrollbar: the wheel moves the offset; press/drag uses the same `VerticalBar`
//!   geometry as painting, so grabbing the thumb middle does not jump.
//! - 文档：Markdown 与 OpenUI 围栏走真实渲染链路；滚轮滚动 `WHEEL_ROWS` 行。
//!   正文按列宽折行；Mermaid 字符图不折行，超出宽度时底部出现可拖动的横向
//!   滚动条，横向滚轮与拖动都能移动视图。GenUI 按钮只展示鼠标按下态，不执行动作。

mod document;
mod list_items;
mod markdown;
mod mermaid;
pub(crate) mod overlay;
pub(crate) mod spinner;
pub(crate) mod streaming;

pub(crate) use self::document::DocumentContent;
use self::document::DocumentLayout;
#[cfg(test)]
pub(crate) use self::document::{
    document_h_scroll_max, document_line_count, document_lines, document_max_scroll,
};
use self::list_items::LIST_ITEMS;
use self::markdown::MARKDOWN_VARIANTS;
use self::streaming::StreamingDocument;

use peri_tui_components::completion::{CompletionPopup, CompletionRow, CompletionStyles};
use peri_tui_components::list::ListMetrics;
use peri_tui_components::list_nav::scroll_start_for_selected;
use peri_tui_components::panel_mouse::is_scrollbar_column;
use peri_tui_components::scrollbar::{VerticalBar, VerticalPress, bounded_offset};
use peri_tui_components::textarea::{TextArea, TextAreaState, viewport_window, wrap_text};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, StatefulWidgetRef, Widget};
use unicode_width::UnicodeWidthChar;

pub(crate) const ACCENT: Color = Color::Cyan;
pub(crate) const BG: Color = Color::Black;
pub(crate) const FG: Color = Color::Gray;
pub(crate) const MUTED: Color = Color::DarkGray;
pub(crate) const BORDER: Style = Style::new().fg(Color::DarkGray);

/// Rows a single wheel notch scrolls; the host uses `SCROLL_LINES` (3 rows).
pub(crate) const WHEEL_ROWS: usize = 3;
/// Columns a horizontal wheel notch scrolls; the bar drag is the precise path.
const WHEEL_COLS: usize = 8;

/// Viewport height assumed when seeding a scrolled variant; rendering clamps the
/// offset against the real preview height.
const LIST_INITIAL_VIEWPORT: usize = 12;

pub(crate) fn selected_style() -> Style {
    Style::new().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD)
}

pub(crate) fn normal_style() -> Style {
    Style::new().fg(FG)
}

pub(crate) fn accent_style() -> Style {
    Style::new().fg(ACCENT)
}

pub(crate) fn muted_style() -> Style {
    Style::new().fg(MUTED)
}

pub(crate) fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    area.width > 0
        && area.height > 0
        && column >= area.x
        && column < area.right()
        && row >= area.y
        && row < area.bottom()
}

pub(crate) enum StoryState {
    TextArea {
        editor: TextAreaState,
        focused: bool,
        width: Option<u16>,
    },
    List {
        selected: usize,
        /// Viewport offset in items, driven only by the wheel — the host keeps
        /// selection and `ScrollView` offset separate.
        scroll: usize,
        header: bool,
        footer: bool,
        /// 首帧按真实视口把选中项滚到底部，只应用一次，之后滚轮自由移动。
        reveal_last: bool,
    },
    Scrollbar {
        /// First visible content row (raw offset, not ratatui's item position).
        position: usize,
        total: usize,
        /// Thumb drag in progress (grab offset inside the thumb).
        grab: Option<u16>,
    },
    /// 内联补全列表：固定输入，点击命中与绘制共用同一个 `CompletionPopup` 窗口。
    Completion {
        fixture: &'static CompletionFixture,
        selected: usize,
    },
    Document {
        content: DocumentContent<'static>,
        /// First rendered row; wheel-driven and clamped to the rendered height.
        scroll: usize,
        /// First rendered column for unwrapped diagrams; bar/wheel driven.
        h_scroll: usize,
        pressed_button: Option<usize>,
        /// Horizontal thumb drag in progress (grab offset inside the thumb).
        h_drag: Option<usize>,
        layout: Option<DocumentLayout>,
    },
    /// 流式文档：只渲染已到达的前缀；进度由拖拽条或 `[ auto ]` 的 3 秒播放决定。
    Streaming(StreamingDocument),
    /// A non-animated tui-chart gallery frame; `demo` indexes `charts::DEMOS`.
    Chart { demo: usize },
    /// 确认 / 下载弹窗：共享外框，确认与滚轮路由留在宿主。
    Overlay(overlay::OverlayState),
    /// 无状态活动指示器：帧序号与帧集由宿主提供。
    Spinner(spinner::SpinnerState),
}

pub(crate) struct Variant {
    pub(crate) key: &'static str,
    pub(crate) note: &'static str,
    pub(crate) initial: fn() -> StoryState,
}

pub(crate) struct Story {
    pub(crate) key: &'static str,
    pub(crate) title: &'static str,
    pub(crate) variants: &'static [Variant],
}

// ---------------------------------------------------------------------------
// Initial states
// ---------------------------------------------------------------------------

fn text_area_state(text: &str, placeholder: &str) -> TextAreaState {
    let mut state = TextAreaState::default();
    if !text.is_empty() {
        state.insert_str(text);
    }
    state.set_placeholder(placeholder);
    state
}

fn textarea_state(text: &str, placeholder: &str, focused: bool, width: Option<u16>) -> StoryState {
    StoryState::TextArea {
        editor: text_area_state(text, placeholder),
        focused,
        width,
    }
}

fn list_state(
    selected: usize,
    scroll: usize,
    header: bool,
    footer: bool,
    reveal_last: bool,
) -> StoryState {
    StoryState::List {
        selected,
        scroll,
        header,
        footer,
        reveal_last,
    }
}

fn scrollbar_state(position: usize, total: usize) -> StoryState {
    StoryState::Scrollbar {
        position,
        total,
        grab: None,
    }
}

fn textarea_placeholder_initial() -> StoryState {
    textarea_state("", "Type a message…  (Enter to send)", false, None)
}

fn textarea_focused_initial() -> StoryState {
    textarea_state("", "Cursor is visible while focused", true, None)
}

fn textarea_cjk_initial() -> StoryState {
    textarea_state(
        "你好，世界\n第二行：多语言输入\nThird line: mixed 中英文 widths",
        "Placeholder",
        true,
        None,
    )
}

fn textarea_wrapped_initial() -> StoryState {
    textarea_state(
        "Peri is a terminal AI coding assistant. This deliberately long line wraps across the preview width, showing soft wrapping, viewport clipping and cursor placement near the end.",
        "Placeholder",
        true,
        None,
    )
}

fn textarea_narrow_initial() -> StoryState {
    textarea_state("narrow\nviewport\n中英文", "Placeholder", true, Some(14))
}

fn list_first_initial() -> StoryState {
    list_state(0, 0, true, true, false)
}

fn list_middle_initial() -> StoryState {
    list_state(
        14,
        scroll_start_for_selected(14, LIST_ITEMS.len(), LIST_INITIAL_VIEWPORT),
        true,
        true,
        false,
    )
}

fn list_last_initial() -> StoryState {
    let last = LIST_ITEMS.len() - 1;
    // 视口高度只在绘制时才知道：首帧把选中项贴到底部。
    list_state(last, 0, true, true, true)
}

fn list_plain_initial() -> StoryState {
    list_state(2, 0, false, false, false)
}

fn scrollbar_top_initial() -> StoryState {
    scrollbar_state(0, 100)
}

fn scrollbar_middle_initial() -> StoryState {
    scrollbar_state(50, 100)
}

fn scrollbar_bottom_initial() -> StoryState {
    scrollbar_state(99, 100)
}

// ---------------------------------------------------------------------------
// Completion fixtures (inline height mirrors the host's theme `inline_height`)
// ---------------------------------------------------------------------------

const COMPLETION_HEIGHT: u16 = 10;

pub(crate) struct CompletionFixture {
    pub(crate) title: &'static str,
    pub(crate) empty_text: &'static str,
    pub(crate) rows: &'static [(&'static str, &'static str, Color)],
}

const COMPLETION_MENTION: CompletionFixture = CompletionFixture {
    title: " @ src/ ",
    empty_text: "No matches",
    rows: &[
        ("src/main.rs", "", FG),
        ("src/lib.rs", "", FG),
        ("src/kit/mod.rs", "", FG),
        ("src/kit/input_area.rs", "", FG),
        ("src/kit/slash_completion.rs", "", FG),
        ("src/kit/mention_popup.rs", "", FG),
        ("Cargo.toml", "", FG),
        ("README.md", "", FG),
    ],
};

const COMPLETION_SLASH: CompletionFixture = CompletionFixture {
    title: " / ",
    empty_text: "No matches",
    rows: &[
        ("/help", "Show help and shortcuts", ACCENT),
        ("/clear", "Clear the transcript", FG),
        ("/compact", "Compact the conversation", FG),
        ("/model", "Switch model profile", FG),
        ("/plugin", "Open the plugin panel", ACCENT),
        ("/skill:review", "Run a packaged skill", Color::Yellow),
    ],
};

const COMPLETION_SCROLLED: CompletionFixture = CompletionFixture {
    title: " /model ",
    empty_text: "No matches",
    rows: &[
        ("/model:default", "Active profile", ACCENT),
        ("/model:review", "Review profile", FG),
        ("/model:fast", "Low latency profile", FG),
        ("/model:deep", "Reasoning profile", FG),
        ("/model:local", "Local provider", FG),
        ("/model:vision", "Image input profile", FG),
        ("/model:plan", "Planning profile", FG),
        ("/model:long", "Large context profile", FG),
        ("/model:cheap", "Budget profile", FG),
        ("/model:staging", "Staging profile", FG),
        ("/model:prod", "Production profile", FG),
        ("/model:experimental", "Opt-in profile", Color::Yellow),
    ],
};

const COMPLETION_EMPTY: CompletionFixture = CompletionFixture {
    title: " /missing ",
    empty_text: "No matches",
    rows: &[],
};

fn completion_state(fixture: &'static CompletionFixture, selected: usize) -> StoryState {
    StoryState::Completion { fixture, selected }
}

fn completion_mention_initial() -> StoryState {
    completion_state(&COMPLETION_MENTION, 1)
}

fn completion_slash_initial() -> StoryState {
    completion_state(&COMPLETION_SLASH, 2)
}

fn completion_scrolled_initial() -> StoryState {
    // 8 行窗口放不下 12 行：max_scroll = 4，选中项 10 仍在窗口内。
    completion_state(&COMPLETION_SCROLLED, 10)
}

fn completion_empty_initial() -> StoryState {
    completion_state(&COMPLETION_EMPTY, 0)
}

fn document_state(source: &'static str, scroll: usize) -> StoryState {
    StoryState::Document {
        content: DocumentContent::Markdown(source),
        scroll,
        h_scroll: 0,
        pressed_button: None,
        h_drag: None,
        layout: None,
    }
}

// ---------------------------------------------------------------------------
// OpenUI fixtures
// ---------------------------------------------------------------------------

const OPENUI_STACK_COLUMN: &str = r#"```gen-ui
root = Stack([
  Text("第一行：column 布局"),
  Text("第二行：子组件按顺序堆叠"),
  Text("第三行：宽度自适应")
], "column")
```
"#;

const OPENUI_STACK_ROW: &str = r#"```gen-ui
root = Stack([
  Text("左"),
  Text("中"),
  Text("右")
], "row")
```
"#;

const OPENUI_CARD: &str = r#"```gen-ui
root = Card([
  Heading("任务概览", 2),
  Text("Card 以边框包裹子组件", "muted"),
  Divider(),
  Text("边框占用宽度时会自动降级")
], "卡片标题")
```
"#;

const OPENUI_TEXT: &str = r#"```gen-ui
root = Stack([
  Text("默认正文 Text"),
  Text("弱化文本", "muted"),
  Text("标题语义", "heading"),
  Text("等宽代码语义", "code")
], "column")
```
"#;

const OPENUI_HEADING: &str = r#"```gen-ui
root = Stack([
  Heading("一级标题", 1),
  Heading("二级标题", 2),
  Heading("三级标题", 3)
], "column")
```
"#;

const OPENUI_LIST: &str = r#"```gen-ui
root = Stack([
  List(["无序甲", "无序乙"]),
  List(["第一步", "第二步", "第三步"], true)
], "column")
```
"#;

const OPENUI_TABLE: &str = r#"```gen-ui
root = Table(["名称", "状态"], [["Markdown", "完成"], ["OpenUI", "完成"], ["Storybook", "交互"]], "组件矩阵")
```
"#;

const OPENUI_KEY_VALUE: &str = r#"```gen-ui
root = KeyValue([{key: "语言", value: "Rust"}, {key: "渲染", value: "ratatui"}, {key: "宽度", value: 80}], "详情")
```
"#;

const OPENUI_PROGRESS: &str = r#"```gen-ui
root = Stack([
  Progress("解析", 70, 100),
  Progress("渲染", 40, 100),
  Progress("完成", 100, 100)
], "column")
```
"#;

const OPENUI_DIVIDER: &str = r#"```gen-ui
root = Stack([
  Text("分段一"),
  Divider(),
  Text("分段二")
], "column")
```
"#;

const OPENUI_BUTTON: &str = r#"```gen-ui
root = Stack([
  Button("继续", @ToAssistant("继续处理"), "primary"),
  Button("查看文档", @OpenUrl("https://example.com"), "secondary")
], "column")
```
"#;

const OPENUI_BAR_CHART: &str = r#"```gen-ui
root = BarChart([{label: "Markdown", value: 42}, {label: "OpenUI", value: 28}, {label: "交互", value: 15}], "每周渲染")
```
"#;

const OPENUI_SPARKLINE: &str = r#"```gen-ui
root = Sparkline([1, 3, 2, 5, 4, 7, 6, 9, 8, 11], "延迟抖动")
```
"#;

fn openui_stack_column_initial() -> StoryState {
    document_state(OPENUI_STACK_COLUMN, 0)
}

fn openui_stack_row_initial() -> StoryState {
    document_state(OPENUI_STACK_ROW, 0)
}

fn openui_card_initial() -> StoryState {
    document_state(OPENUI_CARD, 0)
}

fn openui_text_initial() -> StoryState {
    document_state(OPENUI_TEXT, 0)
}

fn openui_heading_initial() -> StoryState {
    document_state(OPENUI_HEADING, 0)
}

fn openui_list_initial() -> StoryState {
    document_state(OPENUI_LIST, 0)
}

fn openui_table_initial() -> StoryState {
    document_state(OPENUI_TABLE, 0)
}

fn openui_key_value_initial() -> StoryState {
    document_state(OPENUI_KEY_VALUE, 0)
}

fn openui_progress_initial() -> StoryState {
    document_state(OPENUI_PROGRESS, 0)
}

fn openui_divider_initial() -> StoryState {
    document_state(OPENUI_DIVIDER, 0)
}

fn openui_button_initial() -> StoryState {
    document_state(OPENUI_BUTTON, 0)
}

fn openui_bar_chart_initial() -> StoryState {
    document_state(OPENUI_BAR_CHART, 0)
}

fn openui_sparkline_initial() -> StoryState {
    document_state(OPENUI_SPARKLINE, 0)
}

// ---------------------------------------------------------------------------
// Story registry
// ---------------------------------------------------------------------------

const TEXTAREA_VARIANTS: &[Variant] = &[
    Variant {
        key: "placeholder",
        note: "Empty and unfocused; click places the caret at the clicked row/column.",
        initial: textarea_placeholder_initial,
    },
    Variant {
        key: "focused",
        note: "Focused input; clicking anywhere places the caret.",
        initial: textarea_focused_initial,
    },
    Variant {
        key: "cjk",
        note: "CJK text; click mapping uses display width, not byte offsets.",
        initial: textarea_cjk_initial,
    },
    Variant {
        key: "wrapped",
        note: "Soft wrapping; clicking a wrapped row maps back to the source character.",
        initial: textarea_wrapped_initial,
    },
    Variant {
        key: "narrow",
        note: "14-column viewport with the same click-to-caret mapping.",
        initial: textarea_narrow_initial,
    },
];

const LIST_VARIANTS: &[Variant] = &[
    Variant {
        key: "first",
        note: "Click a row to select it (click as enter); the scrollbar column is excluded.",
        initial: list_first_initial,
    },
    Variant {
        key: "middle",
        note: "Click as enter; the wheel scrolls the view, not the selection.",
        initial: list_middle_initial,
    },
    Variant {
        key: "last",
        note: "Click as enter at the end; each wheel notch scrolls three rows.",
        initial: list_last_initial,
    },
    Variant {
        key: "plain",
        note: "No header/footer; every content row is clickable.",
        initial: list_plain_initial,
    },
];

const SCROLLBAR_VARIANTS: &[Variant] = &[
    Variant {
        key: "top",
        note: "Wheel down moves the position; clicking the bar itself is ignored.",
        initial: scrollbar_top_initial,
    },
    Variant {
        key: "middle",
        note: "Wheel changes the position, mirroring ScrollView scrolling.",
        initial: scrollbar_middle_initial,
    },
    Variant {
        key: "bottom",
        note: "Wheel up moves the position; the bar has no click handler.",
        initial: scrollbar_bottom_initial,
    },
];

const COMPLETION_VARIANTS: &[Variant] = &[
    Variant {
        key: "mention",
        note: "Click a row to select it; @ rows have no description column.",
        initial: completion_mention_initial,
    },
    Variant {
        key: "slash",
        note: "Label and description columns; tier colours are written by the host.",
        initial: completion_slash_initial,
    },
    Variant {
        key: "scrolled",
        note: "Eight-row window; clicks map painted rows back to full-list indexes.",
        initial: completion_scrolled_initial,
    },
    Variant {
        key: "empty",
        note: "No candidates: empty text is painted and no row is clickable.",
        initial: completion_empty_initial,
    },
];

const MERMAID_VARIANTS: &[Variant] = &[
    Variant {
        key: "flowchart",
        note: "官方 CI/CD 流程：三个 subgraph 与判定分支。",
        initial: mermaid::flowchart_initial,
    },
    Variant {
        key: "sequence",
        note: "官方支付流程：autonumber、activate 与 alt 分支。",
        initial: mermaid::sequence_initial,
    },
    Variant {
        key: "state",
        note: "官方基础状态机：起点、终态与转移边。",
        initial: mermaid::state_initial,
    },
    Variant {
        key: "class",
        note: "官方电商类图：属性、方法、组合与依赖。",
        initial: mermaid::class_initial,
    },
    Variant {
        key: "er",
        note: "官方基础 ER 图：四实体、属性与基数关系。",
        initial: mermaid::er_initial,
    },
    Variant {
        key: "pie",
        note: "官方饼图：标题、分片标签与占比。",
        initial: mermaid::pie_initial,
    },
    Variant {
        key: "journey",
        note: "官方用户旅程：分组任务与满意度评分。",
        initial: mermaid::journey_initial,
    },
    Variant {
        key: "gantt",
        note: "官方甘特图：日期轴、分组任务与持续时长。",
        initial: mermaid::gantt_initial,
    },
    Variant {
        key: "timeline",
        note: "官方时间线：时间段与多事件。",
        initial: mermaid::timeline_initial,
    },
    Variant {
        key: "gitgraph",
        note: "官方 Git 图：分支、合并与 tag。",
        initial: mermaid::gitgraph_initial,
    },
    Variant {
        key: "mindmap",
        note: "官方思维导图：根节点与多层子树。",
        initial: mermaid::mindmap_initial,
    },
    Variant {
        key: "quadrant",
        note: "官方象限图：坐标轴、四个象限与散点。",
        initial: mermaid::quadrant_initial,
    },
    Variant {
        key: "requirement",
        note: "官方需求图：六类需求、元素与全部关系。",
        initial: mermaid::requirement_initial,
    },
    Variant {
        key: "sankey",
        note: "官方桑基图：来源、去向与流量宽度。",
        initial: mermaid::sankey_initial,
    },
    Variant {
        key: "xychart",
        note: "官方 XY 图：柱状与折线叠加。",
        initial: mermaid::xychart_initial,
    },
    Variant {
        key: "architecture",
        note: "官方架构图：分组、服务与连线。",
        initial: mermaid::architecture_initial,
    },
    Variant {
        key: "packet",
        note: "官方报文图：位区间与字段标签。",
        initial: mermaid::packet_initial,
    },
    Variant {
        key: "streaming",
        note: "拖动进度条或自动播放；只把已确认的语句绘成字符图。",
        initial: mermaid::streaming_initial,
    },
];

const OPENUI_VARIANTS: &[Variant] = &[
    Variant {
        key: "stack-column",
        note: "Stack(column) stacks children vertically with block spacing.",
        initial: openui_stack_column_initial,
    },
    Variant {
        key: "stack-row",
        note: "Stack(row) places children side by side when width allows.",
        initial: openui_stack_row_initial,
    },
    Variant {
        key: "card",
        note: "Card frames children with a titled border and inner padding.",
        initial: openui_card_initial,
    },
    Variant {
        key: "text",
        note: "Text variants: body, muted, heading and code.",
        initial: openui_text_initial,
    },
    Variant {
        key: "heading",
        note: "Heading levels 1-3 use accent styling and bold fallbacks.",
        initial: openui_heading_initial,
    },
    Variant {
        key: "list",
        note: "List renders bullets and ordered numbering.",
        initial: openui_list_initial,
    },
    Variant {
        key: "table",
        note: "Table shares the Markdown unicode grid renderer.",
        initial: openui_table_initial,
    },
    Variant {
        key: "key-value",
        note: "KeyValue prints `key: value` rows under an optional title.",
        initial: openui_key_value_initial,
    },
    Variant {
        key: "progress",
        note: "Progress shows a filled bar with the value/max percentage.",
        initial: openui_progress_initial,
    },
    Variant {
        key: "divider",
        note: "Divider spans the available width with a dim rule.",
        initial: openui_divider_initial,
    },
    Variant {
        key: "button",
        note: "Buttons keep their action descriptors while rendering labels.",
        initial: openui_button_initial,
    },
    Variant {
        key: "bar-chart",
        note: "BarChart renders proportional bars with labels and values.",
        initial: openui_bar_chart_initial,
    },
    Variant {
        key: "sparkline",
        note: "Sparkline compresses a numeric series into one braille-like row.",
        initial: openui_sparkline_initial,
    },
    Variant {
        key: "streaming",
        note: "逐字符到达的 GenUI 围栏：闭合前按文本降级，闭合后渲染组件。",
        initial: streaming::openui_streaming_initial,
    },
];

/// Chart variants are generated from the gallery's demo table so keys and
/// notes keep a single source of truth.
macro_rules! chart_variants {
    ($($demo:literal => $initial:ident),+ $(,)?) => {
        $(fn $initial() -> StoryState {
            StoryState::Chart { demo: $demo }
        })+

        pub(crate) const CHART_VARIANTS: &[Variant] = &[
            $(Variant {
                key: crate::charts::DEMOS[$demo].key,
                note: crate::charts::DEMOS[$demo].note,
                initial: $initial,
            }),+
        ];
    };
}

chart_variants!(
    0 => chart_bars,
    1 => chart_boxplot,
    2 => chart_bubble,
    3 => chart_bullet,
    4 => chart_calendar,
    5 => chart_candlestick,
    6 => chart_chartstack,
    7 => chart_depth,
    8 => chart_funnel,
    9 => chart_gantt,
    10 => chart_gauge,
    11 => chart_heatmap,
    12 => chart_histogram,
    13 => chart_line,
    14 => chart_lollipop,
    15 => chart_paired_bars,
    16 => chart_pie,
    17 => chart_radar,
    18 => chart_radial,
    19 => chart_renko,
    20 => chart_rose,
    21 => chart_sankey,
    22 => chart_slope,
    23 => chart_sparkline,
    24 => chart_stacked_area,
    25 => chart_stream,
    26 => chart_treemap,
    27 => chart_violin,
    28 => chart_waterfall,
    29 => chart_instruments,
    30 => chart_cyber,
    31 => chart_themes,
    32 => chart_showcase,
    33 => chart_interactive,
    34 => chart_bench,
);

pub(crate) const STORIES: &[Story] = &[
    Story {
        key: "textarea",
        title: "TextArea",
        variants: TEXTAREA_VARIANTS,
    },
    Story {
        key: "list",
        title: "List",
        variants: LIST_VARIANTS,
    },
    Story {
        key: "scrollbar",
        title: "Scrollbar",
        variants: SCROLLBAR_VARIANTS,
    },
    Story {
        key: "completion",
        title: "Completion",
        variants: COMPLETION_VARIANTS,
    },
    Story {
        key: "markdown",
        title: "Markdown",
        variants: MARKDOWN_VARIANTS,
    },
    Story {
        key: "openui",
        title: "OpenUI",
        variants: OPENUI_VARIANTS,
    },
    Story {
        key: "mermaid",
        title: "Mermaid",
        variants: MERMAID_VARIANTS,
    },
    Story {
        key: "overlay",
        title: "Overlay",
        variants: overlay::VARIANTS,
    },
    Story {
        key: "spinner",
        title: "Spinner",
        variants: spinner::VARIANTS,
    },
    Story {
        key: "charts",
        title: "tui-chart",
        variants: CHART_VARIANTS,
    },
];

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub(crate) fn render_story(state: &mut StoryState, area: Rect, buf: &mut Buffer) {
    match state {
        StoryState::TextArea {
            editor,
            focused,
            width,
        } => render_textarea(editor, *focused, *width, area, buf),
        StoryState::List {
            selected,
            scroll,
            header,
            footer,
            reveal_last,
        } => render_list(area, buf, *selected, scroll, *header, *footer, reveal_last),
        StoryState::Scrollbar {
            position, total, ..
        } => {
            render_scrollbar(area, buf, *position, *total, area.height);
        }
        StoryState::Completion { fixture, selected } => {
            render_completion(fixture, *selected, area, buf);
        }
        StoryState::Document {
            content,
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        } => document::DocumentView {
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        }
        .render(area, buf, *content),
        StoryState::Streaming(document) => streaming::render(document, area, buf),
        StoryState::Chart { demo } => crate::charts::render_demo(*demo, area, buf),
        StoryState::Overlay(overlay) => overlay::render(overlay, area, buf),
        StoryState::Spinner(spinner) => spinner::render(spinner, area, buf),
    }
}

fn textarea_target_area(area: Rect, width: Option<u16>) -> Rect {
    match width {
        Some(width) => Rect::new(area.x, area.y, width.min(area.width), area.height),
        None => area,
    }
}

fn render_textarea(
    editor: &mut TextAreaState,
    focused: bool,
    width: Option<u16>,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let target = textarea_target_area(area, width);
    let widget = TextArea::new(
        Style::new().fg(BG).bg(ACCENT),
        Style::new().fg(Color::White).bg(Color::DarkGray),
    )
    .show_cursor(focused)
    .base_style(Style::new().fg(FG).bg(BG))
    .placeholder_style(muted_style());
    widget.render_ref(target, buf, editor);

    if width.is_some() {
        let note_x = target.right().saturating_add(2);
        if note_x < area.right() {
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("{} cols", target.width),
                    accent_style(),
                )),
                Line::from(Span::styled("narrow viewport", muted_style())),
            ])
            .render(
                Rect::new(
                    note_x,
                    area.y,
                    area.right().saturating_sub(note_x),
                    area.height,
                ),
                buf,
            );
        }
    }
}

/// Resolve a wheel offset against the preview height. Rendering and hit testing
/// both call this, so painted rows and clickable rows cannot diverge.
fn list_viewport(
    content_rows: u16,
    metrics: ListMetrics,
    count: usize,
    scroll: usize,
) -> (usize, usize) {
    let usable = content_rows
        .saturating_sub(metrics.header_rows)
        .saturating_sub(metrics.footer_rows);
    let visible = (usable / metrics.item_rows.max(1)) as usize;
    let max_scroll = count.saturating_sub(visible);
    (visible, scroll.min(max_scroll))
}

fn render_list(
    area: Rect,
    buf: &mut Buffer,
    selected: usize,
    scroll: &mut usize,
    header: bool,
    footer: bool,
    reveal_last: &mut bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let metrics = ListMetrics::new(u16::from(header), 1, u16::from(footer));
    let count = LIST_ITEMS.len();
    let (visible, _) = list_viewport(area.height, metrics, count, *scroll);
    let max_scroll = count.saturating_sub(visible);
    // 初始底部偏移按真实视口算一次；之后滚轮移动不再被拉回。
    if *reveal_last {
        *reveal_last = false;
        *scroll = max_scroll;
    }
    *scroll = (*scroll).min(max_scroll);
    let scroll = *scroll;
    let content_width = area.width.saturating_sub(1);

    if header && area.height > 0 {
        Line::from(vec![
            Span::styled(
                " Workspace files",
                accent_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {count} items"), muted_style()),
        ])
        .render(Rect::new(area.x, area.y, area.width, 1), buf);
    }

    let content_top = area.y + metrics.header_rows;
    let window = LIST_ITEMS.iter().enumerate().skip(scroll).take(visible);
    for (row, (index, item)) in window.enumerate() {
        let y = content_top + row as u16;
        if y >= area.bottom() {
            break;
        }
        let style = if index == selected {
            selected_style()
        } else {
            normal_style()
        };
        let index_style = if index == selected {
            style
        } else {
            muted_style()
        };
        Line::from(vec![
            Span::styled(format!(" {:>3} ", index + 1), index_style),
            Span::styled(*item, style),
        ])
        .render(Rect::new(area.x, y, content_width, 1), buf);
    }

    if footer {
        let y = area.bottom().saturating_sub(1);
        Line::from(Span::styled(
            format!(" {count} files · selected #{selected}"),
            muted_style(),
        ))
        .render(Rect::new(area.x, y, content_width, 1), buf);
    }

    if count > visible && area.height >= 3 {
        let bar_area = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        // Raw offset and viewport; the same bar paints and hit-tests in the host.
        if let Some(bar) = VerticalBar::new(area.height, visible, count, scroll) {
            bar.paint(bar_area, buf, Style::new().bg(ACCENT), muted_style());
        }
    }
}

/// Fixed viewport height for the scrollbar story so variants stay comparable.
const SCROLLBAR_VIEWPORT: usize = 10;

fn render_scrollbar(area: Rect, buf: &mut Buffer, offset: usize, total: usize, height: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let bar_area = Rect::new(area.x, area.y, 1, height);
    if let Some(bar) = VerticalBar::new(height, SCROLLBAR_VIEWPORT, total, offset) {
        bar.paint(bar_area, buf, Style::new().bg(ACCENT), muted_style());
    }
    let text_x = area.x.saturating_add(3);
    if text_x < area.right() {
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("offset {offset} / {total} · viewport {SCROLLBAR_VIEWPORT}"),
                accent_style(),
            )),
            Line::from(Span::styled(
                "VerticalBar::new(height, viewport, content, offset)",
                muted_style(),
            )),
            Line::from(Span::styled(
                "paint + press share thumb geometry · drag the thumb",
                muted_style(),
            )),
        ])
        .render(
            Rect::new(
                text_x,
                area.y,
                area.right().saturating_sub(text_x),
                area.height,
            ),
            buf,
        );
    }
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

fn completion_popup(
    fixture: &'static CompletionFixture,
    selected: usize,
    height: u16,
) -> CompletionPopup<'static> {
    let rows = fixture
        .rows
        .iter()
        .map(|(label, detail, color)| {
            let label = CompletionRow::new(Line::from(Span::styled(
                (*label).to_string(),
                Style::new().fg(*color),
            )));
            if detail.is_empty() {
                label
            } else {
                label.with_detail(Line::from(Span::styled(
                    (*detail).to_string(),
                    muted_style(),
                )))
            }
        })
        .collect();
    CompletionPopup {
        rows,
        selected,
        height,
        title: Line::from(Span::styled(
            fixture.title,
            accent_style().add_modifier(Modifier::BOLD),
        )),
        empty_text: Line::from(Span::styled(fixture.empty_text, muted_style())),
        styles: CompletionStyles {
            border: MUTED,
            selected_label: Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            selected_detail: Style::new().fg(ACCENT),
            empty: muted_style(),
        },
    }
}

/// 弹窗占据预览区顶部 `COMPLETION_HEIGHT` 行——与宿主的内联高度一致。
fn completion_target(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width,
        COMPLETION_HEIGHT.min(area.height),
    )
}

fn render_completion(
    fixture: &'static CompletionFixture,
    selected: usize,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let target = completion_target(area);
    completion_popup(fixture, selected, target.height)
        .widget()
        .render(target, buf);
    let note_y = target.bottom().saturating_add(1);
    if note_y < area.bottom() {
        Line::from(Span::styled(
            format!(
                "inline height {} · {} rows · selected #{selected}",
                target.height,
                fixture.rows.len()
            ),
            muted_style(),
        ))
        .render(Rect::new(area.x, note_y, area.width, 1), buf);
    }
}

// ---------------------------------------------------------------------------
// Mouse semantics
// ---------------------------------------------------------------------------

pub(crate) fn handle_story_mouse(state: &mut StoryState, mouse: &MouseEvent, area: Rect) -> bool {
    if !matches!(
        state,
        StoryState::Document { .. } | StoryState::Streaming(_)
    ) && !rect_contains(area, mouse.column, mouse.row)
    {
        return false;
    }
    match state {
        StoryState::TextArea {
            editor,
            focused,
            width,
        } => {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return false;
            }
            let target = textarea_target_area(area, *width);
            if !rect_contains(target, mouse.column, mouse.row) {
                return false;
            }
            *focused = true;
            place_caret(editor, mouse, target);
            true
        }
        StoryState::List {
            selected,
            scroll,
            header,
            footer,
            ..
        } => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                click_list(area, mouse, selected, *scroll, *header, *footer);
                true
            }
            MouseEventKind::ScrollUp => {
                scroll_list(area, *header, *footer, scroll, -(WHEEL_ROWS as isize));
                true
            }
            MouseEventKind::ScrollDown => {
                scroll_list(area, *header, *footer, scroll, WHEEL_ROWS as isize);
                true
            }
            _ => false,
        },
        StoryState::Scrollbar {
            position,
            total,
            grab,
        } => {
            let max_offset = total.saturating_sub(SCROLLBAR_VIEWPORT);
            let bar_area = Rect::new(area.x, area.y, 1, area.height);
            let bar = VerticalBar::new(area.height, SCROLLBAR_VIEWPORT, *total, *position);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    *position = bounded_offset(*position, -(WHEEL_ROWS as i32), max_offset);
                    true
                }
                MouseEventKind::ScrollDown => {
                    *position = bounded_offset(*position, WHEEL_ROWS as i32, max_offset);
                    true
                }
                MouseEventKind::Down(MouseButton::Left) if mouse.column == bar_area.x => {
                    *grab = None;
                    if let Some(bar) = bar {
                        match bar.press(bar_area, mouse.row) {
                            VerticalPress::Top => *position = 0,
                            VerticalPress::Bottom => *position = max_offset,
                            VerticalPress::Thumb { grab: held } => *grab = Some(held),
                            VerticalPress::Track { offset } => {
                                *position = offset;
                                *grab = Some((bar.thumb_len / 2) as u16);
                            }
                        }
                    }
                    true
                }
                MouseEventKind::Drag(MouseButton::Left) if grab.is_some() => {
                    if let Some(bar) = bar {
                        *position =
                            bar.drag_offset(bar_area, mouse.row, grab.expect("checked by guard"));
                    }
                    true
                }
                MouseEventKind::Up(MouseButton::Left) => grab.take().is_some(),
                _ => false,
            }
        }
        StoryState::Completion { fixture, selected } => {
            let target = completion_target(area);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if !rect_contains(target, mouse.column, mouse.row) {
                        return false;
                    }
                    // 与绘制同一个 popup 实例：命中下标就是被画出的那一行。
                    let popup = completion_popup(fixture, *selected, target.height);
                    if let Some(index) = popup.hit(mouse, target) {
                        *selected = index;
                    }
                    true
                }
                MouseEventKind::ScrollUp => {
                    *selected = selected.saturating_sub(WHEEL_ROWS);
                    true
                }
                MouseEventKind::ScrollDown => {
                    *selected = (*selected + WHEEL_ROWS).min(fixture.rows.len().saturating_sub(1));
                    true
                }
                _ => rect_contains(target, mouse.column, mouse.row),
            }
        }
        StoryState::Document {
            content,
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        } => document::DocumentView {
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        }
        .handle_mouse(area, mouse, *content),
        StoryState::Streaming(document) => streaming::handle_mouse(document, mouse, area),
        // Chart frames are static; clicks are consumed like document clicks.
        StoryState::Chart { .. } => matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)),
        StoryState::Overlay(overlay) => overlay::handle_mouse(overlay, mouse, area),
        StoryState::Spinner(spinner) => spinner::handle_mouse(spinner, mouse, area),
    }
}

pub(crate) fn place_caret(editor: &mut TextAreaState, mouse: &MouseEvent, area: Rect) {
    if editor.text.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let visual_row = mouse.row.saturating_sub(area.y) as usize;
    let display_col = mouse.column.saturating_sub(area.x) as usize;
    let max_width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped = wrap_text(&editor.text, editor.cursor, max_width);
    // 渲染器在文本超出视口时以光标行为中心裁剪窗口，命中测试必须用同一窗口，
    // 否则滚动态下点击会落到错误源码行。
    let (start, end) = viewport_window(
        wrapped.total_visual_rows,
        wrapped.cursor_visual_row,
        area.height as usize,
    );
    let source_row = start + visual_row;
    if source_row >= end {
        return;
    }
    let line = &wrapped.visual_lines[source_row];
    let mut column = 0usize;
    let mut target = line.char_range.0;
    for (offset, ch) in line.text.char_indices() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if column + char_width > display_col {
            break;
        }
        column += char_width;
        target = line.char_range.0 + line.text[..offset + ch.len_utf8()].chars().count();
    }
    editor.desired_col = None;
    editor.cursor = target;
}

/// Wheel scroll: move the viewport and leave the selection alone. The host drives a
/// `ScrollView` offset from the wheel while `selected` stays untouched.
fn scroll_list(area: Rect, header: bool, footer: bool, scroll: &mut usize, delta: isize) {
    let metrics = ListMetrics::new(u16::from(header), 1, u16::from(footer));
    let (visible, current) = list_viewport(area.height, metrics, LIST_ITEMS.len(), *scroll);
    let max_scroll = LIST_ITEMS.len().saturating_sub(visible);
    *scroll = (current as isize + delta).clamp(0, max_scroll as isize) as usize;
}

fn click_list(
    area: Rect,
    mouse: &MouseEvent,
    selected: &mut usize,
    scroll: usize,
    header: bool,
    footer: bool,
) {
    if is_scrollbar_column(mouse, area) {
        return;
    }
    let count = LIST_ITEMS.len();
    if count == 0 {
        return;
    }
    let metrics = ListMetrics::new(u16::from(header), 1, u16::from(footer));
    let (visible, scroll) = list_viewport(area.height, metrics, count, scroll);
    let row = mouse.row.saturating_sub(area.y);
    if row < metrics.header_rows {
        return;
    }
    let rendered = count.saturating_sub(scroll).min(visible) as u16;
    let footer_start = metrics
        .header_rows
        .saturating_add(rendered.saturating_mul(metrics.item_rows));
    if row >= footer_start {
        return;
    }
    let in_view = (row - metrics.header_rows) / metrics.item_rows.max(1);
    let index = scroll + in_view as usize;
    if index < count {
        *selected = index;
    }
}
