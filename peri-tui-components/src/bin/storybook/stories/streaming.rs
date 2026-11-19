//! Streaming 变体：Markdown / OpenUI 文档按固定比例渐进到达。
//!
//! dump 只绘制一帧，因此初始进度是源码全长的固定百分比——两次运行画面一致。
//! 交互运行时没有自动播放：进度只由底部拖拽条 seek，或点击 `[ auto ]` 用 3 秒播到 100%。
//! 渲染只取 `source[..cursor]`，其余仍走真实 parse → segment → line 管线。
//! 预览区底部单列一行进度条：鼠标按下/拖拽把横向比例映射回源码偏移。

use std::time::Instant;

use peri_tui_components::button::ButtonChrome;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::StoryState;
use super::document::{self, DocumentLayout};
use super::{accent_style, muted_style, rect_contains, selected_style};

/// 进度条占用的预览区行数；高度不足两行时整块都留给文档。
const PROGRESS_ROWS: u16 = 1;
/// 轨道右侧固定宽度的百分比标签，右对齐显示 `{:>4}%`。
const LABEL_WIDTH: u16 = 5;
/// `[ auto ]` 从当前位置播到末尾的 wall clock 时长。
const AUTO_SECONDS: f64 = 3.0;
/// `[ auto ]` 的显示宽度，与 [`auto_text`] 的输出一致。
const AUTO_WIDTH: u16 = 8;
/// 按钮 + 1 列间隔 + 百分比 + 至少 6 列轨道才显示按钮。
const FULL_MIN_WIDTH: u16 = AUTO_WIDTH + 1 + LABEL_WIDTH + 6;
/// 百分比 + 至少 4 列轨道才显示标签（无按钮）。
const LABEL_MIN_WIDTH: u16 = LABEL_WIDTH + 4;
/// 未到达的轨道字符与已到达的填充字符。
const TRACK: &str = "░";
const FILLED: &str = "█";

/// 全功能 Markdown 文档：标题、行内样式、列表、代码块与表格都会逐块到达。
const MARKDOWN_STREAMING: &str = r#"# 流式 Markdown

逐块到达的富文本：标题、列表、表格与代码块在闭合后立即渲染。

**粗体**、*斜体*、***粗斜体***、~~删除线~~ 与 `inline code` 同批到达。

## 列表

- 第一项：解析器按块提交
- 第二项：**增量**渲染
- 第三项：`code` 保持行内样式

## 代码块

```rust
fn stream(chunk: &str) -> String {
    render(chunk.to_owned())
}
```

## 表格

| 阶段 | 状态 | 帧 |
| --- | --- | ---: |
| 解析 | 完成 | 1 |
| 渲染 | 进行中 | 2 |

## 结尾

围栏闭合后继续推进，直到整篇文档抵达末尾。
"#;

/// GenUI 程序逐字符增长：围栏未闭合时按文本降级，闭合后渲染成真实组件。
const OPENUI_STREAMING: &str = r#"## 流式 GenUI

围栏尚未闭合时按文本降级，闭合后渲染成真实组件。

```gen-ui
root = Stack([
  Card([
    Heading("流式卡片", 2),
    Text("逐字符增长的 GenUI 程序", "muted"),
    Progress("生成进度", 60, 100),
    Button("继续", @ToAssistant("继续"), "primary")
  ], "GenUI")
])
```
"#;

/// 一次自动播放：wall clock 起点与起始偏移；终点恒为源码末尾。
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutoPlay {
    pub(crate) start: Instant,
    pub(crate) from: usize,
}

/// 流式进度：`source[..cursor]` 是已到达的前缀，其余状态与普通文档一致。
pub(crate) struct StreamingDocument {
    source: &'static str,
    /// 已到达的字节偏移；始终落在字符边界上。
    cursor: usize,
    scroll: usize,
    pressed_button: Option<usize>,
    layout: Option<DocumentLayout>,
    /// 进度条上的左键拖拽中：只有此时散落的 `Drag` 才改位置。
    dragging: bool,
    /// `[ auto ]` 按钮按下未松开：只影响按钮反色。
    auto_pressed: bool,
    /// 播放中的线性插值；`None` 表示静止。
    auto: Option<AutoPlay>,
}

fn streaming_state(source: &'static str, percent: usize) -> StoryState {
    StoryState::Streaming(StreamingDocument {
        source,
        cursor: source.floor_char_boundary(source.len() * percent / 100),
        scroll: 0,
        pressed_button: None,
        layout: None,
        dragging: false,
        auto_pressed: false,
        auto: None,
    })
}

pub(super) fn markdown_streaming_initial() -> StoryState {
    streaming_state(MARKDOWN_STREAMING, 58)
}

pub(super) fn openui_streaming_initial() -> StoryState {
    streaming_state(OPENUI_STREAMING, 70)
}

// ---------------------------------------------------------------------------
// 进度条几何
// ---------------------------------------------------------------------------

/// 进度条行：预览区最后一行，右端留一列给文档的纵向滚动条。
fn progress_row(area: Rect) -> Option<Rect> {
    // 至少留一行给文档；高度不足时整块都归文档。
    (area.height > PROGRESS_ROWS)
        .then(|| Rect::new(area.x, area.bottom() - 1, area.width.saturating_sub(1), 1))
}

/// 文档区域：进度条行让出去；高度不足两行时整块都是文档。
fn document_area(area: Rect) -> Rect {
    let height = match progress_row(area) {
        Some(_) => area.height - PROGRESS_ROWS,
        None => area.height,
    };
    Rect::new(area.x, area.y, area.width, height)
}

/// 一行的自适应分区：按钮（行足够宽才有）、轨道与右侧百分比标签。
struct ProgressLayout {
    auto: Option<Rect>,
    bar: Rect,
    label: bool,
}

/// 全部阈值只看 `row.width`：宽行给按钮让位，窄行先丢按钮、再丢标签。
fn progress_layout(row: Rect) -> ProgressLayout {
    if row.width >= FULL_MIN_WIDTH {
        ProgressLayout {
            auto: Some(Rect::new(row.x, row.y, AUTO_WIDTH, 1)),
            bar: Rect::new(
                row.x + AUTO_WIDTH + 1,
                row.y,
                row.width - AUTO_WIDTH - 1 - LABEL_WIDTH,
                1,
            ),
            label: true,
        }
    } else if row.width >= LABEL_MIN_WIDTH {
        ProgressLayout {
            auto: None,
            bar: Rect::new(row.x, row.y, row.width - LABEL_WIDTH, 1),
            label: true,
        }
    } else {
        ProgressLayout {
            auto: None,
            bar: Rect::new(row.x, row.y, row.width, 1),
            label: false,
        }
    }
}

/// 按钮原语给出的 `[ auto ]` 文本；宽度必须等于 `AUTO_WIDTH`。
fn auto_text() -> String {
    ButtonChrome::Bracketed { padded: true }.text("auto")
}

/// 测试用：直接从预览区推轨迹几何，避免测试重复推算行/列偏移。
#[cfg(test)]
pub(crate) fn test_bar_area(area: Rect) -> Rect {
    progress_row(area)
        .map(progress_layout)
        .map(|layout| layout.bar)
        .expect("预览区高度 ≥ 2 才有进度条")
}

/// 测试用：`(auto 按钮, 轨道, 是否显示百分比)`，按真实行宽推布局。
#[cfg(test)]
pub(crate) fn test_progress_layout(area: Rect) -> (Option<Rect>, Rect, bool) {
    let layout = progress_row(area)
        .map(progress_layout)
        .expect("预览区高度 ≥ 2 才有进度条");
    (layout.auto, layout.bar, layout.label)
}

/// 已到达比例，四舍五入到整数百分比；空源码按 0% 处理。
fn percent(source: &str, cursor: usize) -> usize {
    if source.is_empty() {
        return 0;
    }
    (cursor * 100 + source.len() / 2) / source.len()
}

/// 已到达列数：与百分比同口径的四舍五入，100% 时整条填满。
fn filled_columns(source: &str, cursor: usize, width: u16) -> usize {
    if source.is_empty() {
        return 0;
    }
    ((usize::from(width) * cursor + source.len() / 2) / source.len()).min(usize::from(width))
}

/// 列 → 源码字节偏移：轨道最右一格代表 100%，左侧越界为 0，落点回退到字符边界。
fn cursor_at(source: &str, bar: Rect, column: u16) -> usize {
    let width = usize::from(bar.width);
    if width == 0 {
        return 0;
    }
    let offset = if column < bar.x {
        0
    } else {
        (usize::from(column - bar.x) + 1).min(width)
    };
    source.floor_char_boundary(source.len() * offset / width)
}

/// 位置变化后缓存的布局行数与按钮热区都失效；下次渲染重新解析。
fn set_cursor(state: &mut StreamingDocument, cursor: usize) {
    if cursor != state.cursor {
        state.cursor = cursor;
        state.layout = None;
    }
}

// ---------------------------------------------------------------------------
// 渲染与鼠标
// ---------------------------------------------------------------------------

pub(super) fn render(state: &mut StreamingDocument, area: Rect, buf: &mut Buffer) {
    let source = state.source;
    document::render(
        document_area(area),
        buf,
        &source[..state.cursor],
        &mut state.scroll,
        &mut state.pressed_button,
        &mut state.layout,
    );
    if let Some(row) = progress_row(area) {
        render_progress(state, row, buf);
    }
}

fn render_progress(state: &StreamingDocument, row: Rect, buf: &mut Buffer) {
    let layout = progress_layout(row);
    if let Some(auto) = layout.auto {
        let style = if state.auto_pressed {
            selected_style()
        } else {
            accent_style()
        };
        buf.set_string(auto.x, auto.y, auto_text(), style);
    }
    if layout.bar.width > 0 {
        let filled = filled_columns(state.source, state.cursor, layout.bar.width);
        buf.set_string(
            layout.bar.x,
            layout.bar.y,
            FILLED.repeat(filled),
            accent_style(),
        );
        buf.set_string(
            layout.bar.x + filled as u16,
            layout.bar.y,
            TRACK.repeat(usize::from(layout.bar.width) - filled),
            muted_style(),
        );
    }
    if layout.label {
        buf.set_string(
            row.right() - LABEL_WIDTH,
            row.y,
            format!("{:>4}%", percent(state.source, state.cursor)),
            accent_style(),
        );
    }
}

pub(super) fn handle_mouse(state: &mut StreamingDocument, mouse: &MouseEvent, area: Rect) -> bool {
    let row = progress_row(area);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            state.dragging = false;
            state.auto_pressed = false;
            if let Some(row) = row
                && rect_contains(row, mouse.column, mouse.row)
            {
                let layout = progress_layout(row);
                // 进度条行内的按下只改进度：不触碰文档点击态与滚动位置。
                state.pressed_button = None;
                if let Some(auto) = layout.auto
                    && rect_contains(auto, mouse.column, mouse.row)
                {
                    state.auto_pressed = true;
                    start_auto(state);
                    return true;
                }
                // 拖拽优先：按住轨道立刻取消播放，再按列 seek。
                // 轨道右侧的百分比标签不是点击区，避免误点跳到 100%。
                if !rect_contains(layout.bar, mouse.column, mouse.row) {
                    return true;
                }
                state.auto = None;
                state.dragging = true;
                set_cursor(state, cursor_at(state.source, layout.bar, mouse.column));
                return true;
            }
            handle_document(state, mouse, area)
        }
        // 只有拖拽中的 Drag 生效；行外（含纵向越界）仍按列比例 clamp。
        MouseEventKind::Drag(MouseButton::Left) => {
            if state.auto_pressed {
                // 拖出按钮即恢复未按下态；拖回来重新按下（播放已开始，不会重启）。
                let auto_rect = row.map(progress_layout).and_then(|layout| layout.auto);
                state.auto_pressed =
                    auto_rect.is_some_and(|auto| rect_contains(auto, mouse.column, mouse.row));
                return true;
            }
            if !state.dragging {
                return false;
            }
            state.pressed_button = None;
            if let Some(row) = row {
                let bar = progress_layout(row).bar;
                set_cursor(state, cursor_at(state.source, bar, mouse.column));
            }
            true
        }
        MouseEventKind::Up(MouseButton::Left) if state.auto_pressed => {
            state.auto_pressed = false;
            true
        }
        MouseEventKind::Up(MouseButton::Left) if state.dragging => {
            state.dragging = false;
            state.pressed_button = None;
            true
        }
        _ => handle_document(state, mouse, area),
    }
}

/// 进度条之外的鼠标事件仍完全交给文档处理，只是区域少了一行。
fn handle_document(state: &mut StreamingDocument, mouse: &MouseEvent, area: Rect) -> bool {
    let source = state.source;
    document::handle_mouse(
        document_area(area),
        mouse,
        &source[..state.cursor],
        &mut state.scroll,
        &mut state.pressed_button,
        &mut state.layout,
    )
}

/// 从当前 cursor 起播；已在 100% 时先回到 0，再从 0 播到末尾。
fn start_auto(state: &mut StreamingDocument) {
    if state.cursor >= state.source.len() {
        set_cursor(state, 0);
    }
    state.auto = Some(AutoPlay {
        start: Instant::now(),
        from: state.cursor,
    });
}

/// 是否有正在播放的自动进度（只有它需要缩短事件轮询间隔）。
pub(crate) fn is_animating(state: &StoryState) -> bool {
    matches!(state, StoryState::Streaming(document) if document.auto.is_some())
}

/// 按 wall clock 把播放比例折算成 cursor；到点后清空播放态，之后 tick 无副作用。
pub(crate) fn tick_at(state: &mut StoryState, now: Instant) {
    let StoryState::Streaming(document) = state else {
        return;
    };
    let Some(auto) = document.auto else {
        return;
    };
    let fraction =
        (now.saturating_duration_since(auto.start).as_secs_f64() / AUTO_SECONDS).min(1.0);
    let len = document.source.len();
    let target = auto.from as f64 + (len - auto.from) as f64 * fraction;
    let cursor = document
        .source
        .floor_char_boundary((target as usize).min(len));
    set_cursor(document, cursor);
    if fraction >= 1.0 {
        document.auto = None;
    }
}

/// 测试用：不做匹配模板，直接读播放态。
#[cfg(test)]
pub(crate) fn autoplay(state: &StoryState) -> Option<AutoPlay> {
    match state {
        StoryState::Streaming(document) => document.auto,
        _ => None,
    }
}

#[cfg(test)]
impl StreamingDocument {
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn source(&self) -> &'static str {
        self.source
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn pressed_button(&self) -> Option<usize> {
        self.pressed_button
    }

    pub(crate) fn auto_pressed(&self) -> bool {
        self.auto_pressed
    }
}
