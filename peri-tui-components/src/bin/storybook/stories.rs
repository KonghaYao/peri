//! Story fixtures, rendering and mouse semantics for the TUI storybook.
//!
//! Interaction mirrors the host TUI:
//! - TextArea: left-click places the caret using wrapped visual row/display column.
//! - List: left-click is "click as enter" (selection); header/footer rows are inert and
//!   the scrollbar column is excluded; the wheel scrolls the viewport by `WHEEL_ROWS`
//!   (the host's `SCROLL_LINES`) without moving the selection.
//! - Scrollbar: the wheel moves the position; clicking the bar itself is ignored.
//! - 文档：Markdown 与 OpenUI 围栏走真实渲染链路；滚轮滚动 `WHEEL_ROWS` 行。
//!   GenUI 按钮只展示鼠标按下态，不执行动作。

mod document;
mod list_items;
mod markdown;
mod mermaid;
pub(crate) mod streaming;

pub(crate) use self::document::DocumentContent;
use self::document::DocumentLayout;
#[cfg(test)]
pub(crate) use self::document::{document_lines, document_max_scroll};
use self::list_items::LIST_ITEMS;
use self::markdown::MARKDOWN_VARIANTS;
use self::streaming::StreamingDocument;

use peri_tui_components::list::ListMetrics;
use peri_tui_components::list_nav::scroll_start_for_selected;
use peri_tui_components::panel_mouse::is_scrollbar_column;
use peri_tui_components::scrollbar::vertical_scrollbar;
use peri_tui_components::textarea::{TextArea, TextAreaState, viewport_window, wrap_text};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, ScrollbarState, StatefulWidget, StatefulWidgetRef, Widget};
use unicode_width::UnicodeWidthChar;

pub(crate) const ACCENT: Color = Color::Cyan;
pub(crate) const BG: Color = Color::Black;
pub(crate) const FG: Color = Color::Gray;
pub(crate) const MUTED: Color = Color::DarkGray;
pub(crate) const BORDER: Style = Style::new().fg(Color::DarkGray);

/// Rows a single wheel notch scrolls; the host uses `SCROLL_LINES` (3 rows).
const WHEEL_ROWS: usize = 3;

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
    },
    Scrollbar {
        position: usize,
        total: usize,
    },
    Document {
        content: DocumentContent<'static>,
        /// First rendered row; wheel-driven and clamped to the rendered height.
        scroll: usize,
        pressed_button: Option<usize>,
        layout: Option<DocumentLayout>,
    },
    /// 流式文档：只渲染已到达的前缀；进度由拖拽条或 `[ auto ]` 的 3 秒播放决定。
    Streaming(StreamingDocument),
    /// A non-animated tui-chart gallery frame; `demo` indexes `charts::DEMOS`.
    Chart {
        demo: usize,
    },
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

fn list_state(selected: usize, scroll: usize, header: bool, footer: bool) -> StoryState {
    StoryState::List {
        selected,
        scroll,
        header,
        footer,
    }
}

fn scrollbar_state(position: usize, total: usize) -> StoryState {
    StoryState::Scrollbar { position, total }
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
    list_state(0, 0, true, true)
}

fn list_middle_initial() -> StoryState {
    list_state(
        14,
        scroll_start_for_selected(14, LIST_ITEMS.len(), LIST_INITIAL_VIEWPORT),
        true,
        true,
    )
}

fn list_last_initial() -> StoryState {
    let last = LIST_ITEMS.len() - 1;
    list_state(
        last,
        scroll_start_for_selected(last, LIST_ITEMS.len(), LIST_INITIAL_VIEWPORT),
        true,
        true,
    )
}

fn list_plain_initial() -> StoryState {
    list_state(2, 0, false, false)
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

fn document_state(source: &'static str, scroll: usize) -> StoryState {
    StoryState::Document {
        content: DocumentContent::Markdown(source),
        scroll,
        pressed_button: None,
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

const MERMAID_VARIANTS: &[Variant] = &[
    Variant {
        key: "flowchart",
        note: "Small flowchart rendered with the Mermaid frame API.",
        initial: mermaid::flowchart_initial,
    },
    Variant {
        key: "sequence",
        note: "Sequence diagram rendered as terminal characters.",
        initial: mermaid::sequence_initial,
    },
    Variant {
        key: "collaboration",
        note: "Large collaboration flowchart uses the semantic overview path.",
        initial: mermaid::collaboration_initial,
    },
    Variant {
        key: "dense",
        note: "密集流图：以语义概览保留分组和边的信息。",
        initial: mermaid::dense_initial,
    },
    Variant {
        key: "long-workflow",
        note: "Long workflow remains bounded by the overview renderer.",
        initial: mermaid::long_workflow_initial,
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
        } => render_list(area, buf, *selected, *scroll, *header, *footer),
        StoryState::Scrollbar { position, total } => {
            render_scrollbar(area, buf, *position, *total);
        }
        StoryState::Document {
            content,
            scroll,
            pressed_button,
            layout,
        } => document::render(area, buf, *content, scroll, pressed_button, layout),
        StoryState::Streaming(document) => streaming::render(document, area, buf),
        StoryState::Chart { demo } => crate::charts::render_demo(*demo, area, buf),
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
    scroll: usize,
    header: bool,
    footer: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let metrics = ListMetrics::new(u16::from(header), 1, u16::from(footer));
    let count = LIST_ITEMS.len();
    let (visible, scroll) = list_viewport(area.height, metrics, count, scroll);
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
        let mut state = ScrollbarState::new(count.saturating_sub(visible)).position(scroll);
        StatefulWidget::render(
            vertical_scrollbar(Style::new().bg(ACCENT), muted_style()),
            bar_area,
            buf,
            &mut state,
        );
    }
}

fn render_scrollbar(area: Rect, buf: &mut Buffer, position: usize, total: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let bar_area = Rect::new(area.x, area.y, 1, area.height);
    let mut state = ScrollbarState::new(total).position(position);
    StatefulWidget::render(
        vertical_scrollbar(Style::new().bg(ACCENT), muted_style()),
        bar_area,
        buf,
        &mut state,
    );
    let text_x = area.x.saturating_add(3);
    if text_x < area.right() {
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("position {position} / {total}"),
                accent_style(),
            )),
            Line::from(Span::styled(
                "vertical_scrollbar(thumb, arrow)",
                muted_style(),
            )),
            Line::from(Span::styled(
                "track_symbol(None), ▲/▼ arrow heads",
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
        StoryState::Scrollbar { position, total } => match mouse.kind {
            MouseEventKind::ScrollUp => {
                *position = position.saturating_sub(WHEEL_ROWS);
                true
            }
            MouseEventKind::ScrollDown => {
                *position = (*position + WHEEL_ROWS).min(total.saturating_sub(1));
                true
            }
            // The real TUI excludes the scrollbar column from list clicks; the bar
            // itself has no click handler.
            MouseEventKind::Down(MouseButton::Left) => true,
            _ => false,
        },
        StoryState::Document {
            content,
            scroll,
            pressed_button,
            layout,
        } => document::handle_mouse(area, mouse, *content, scroll, pressed_button, layout),
        StoryState::Streaming(document) => streaming::handle_mouse(document, mouse, area),
        // Chart frames are static; clicks are consumed like document clicks.
        StoryState::Chart { .. } => matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)),
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
