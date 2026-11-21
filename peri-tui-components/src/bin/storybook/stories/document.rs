use std::ops::Range;

use peri_theme::{bridge::ThemeDefinitionExt, builtin::dark_theme};
use peri_tui_components::button::hit_test;
use peri_tui_components::genui;
use peri_tui_components::h_viewport::HorizontalViewport;
use peri_tui_components::markdown::{MarkdownSegment, parse_markdown, table_data_to_lines};
use peri_tui_components::scrollbar::{HorizontalBar, horizontal_scrollbar, vertical_scrollbar};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, ScrollbarState, StatefulWidget, Widget};
use ratatui_kit::{ComponentTheme, components::TableTheme};

use super::{MUTED, WHEEL_COLS, WHEEL_ROWS, mermaid, muted_style, rect_contains};

#[derive(Clone, Copy)]
pub(crate) enum DocumentContent<'a> {
    Markdown(&'a str),
    Mermaid { source: &'a str, terminal: bool },
}

impl<'a> DocumentContent<'a> {
    pub(super) fn text(self) -> &'a str {
        match self {
            Self::Markdown(source) | Self::Mermaid { source, .. } => source,
        }
    }

    pub(super) fn prefix(self, cursor: usize) -> Self {
        match self {
            Self::Markdown(source) => Self::Markdown(&source[..cursor]),
            Self::Mermaid { source, terminal } => Self::Mermaid {
                source: &source[..cursor],
                terminal: terminal && cursor == source.len(),
            },
        }
    }
}

struct DocumentButton {
    rows: Range<usize>,
    x_start: u16,
    x_end: u16,
}

impl DocumentButton {
    fn screen_rect(&self, area: Rect, scroll: usize) -> Rect {
        let top = self
            .rows
            .start
            .saturating_sub(scroll)
            .min(area.height as usize) as u16;
        let bottom = self
            .rows
            .end
            .saturating_sub(scroll)
            .min(area.height as usize) as u16;
        let left = self.x_start.min(area.width);
        let right = self.x_end.min(area.width);
        Rect::new(
            area.x.saturating_add(left),
            area.y.saturating_add(top),
            right - left,
            bottom - top,
        )
    }
}

pub(crate) struct DocumentLayout {
    size: (usize, usize),
    lines: Vec<Line<'static>>,
    buttons: Vec<DocumentButton>,
    content_width: usize,
    content_height: usize,
    max_scroll: usize,
    h_viewport: HorizontalViewport,
    /// 底部横向滚动条是否真的占到一行（视口只有一行或放不下箭头时不显示）。
    has_h_bar: bool,
}

impl DocumentLayout {
    fn new(content: DocumentContent<'_>, width: usize, height: usize) -> Self {
        let layout = |width| match content {
            DocumentContent::Markdown(source) => layout_lines(source, width),
            DocumentContent::Mermaid { source, terminal } => {
                (mermaid::lines(source, terminal, width), Vec::new())
            }
        };
        let mut content_width = width.max(1);
        let mut content_height = height.max(1);
        // 纵向滚动条占一列、横向滚动条占一行；让位后重新排版可能又产生溢出，
        // 所以循环到两者都不再变化。滚动条本身最多各让一次。
        let mut vertical = false;
        let mut horizontal = false;
        let mut widest;
        let (lines, buttons) = loop {
            let (lines, buttons) = layout(content_width);
            widest = lines.iter().map(Line::width).max().unwrap_or(0);
            if lines.len() > content_height && !vertical && content_width > 1 {
                vertical = true;
                content_width -= 1;
                continue;
            }
            if widest > content_width && !horizontal && content_height > 1 {
                horizontal = true;
                content_height -= 1;
                continue;
            }
            break (lines, buttons);
        };
        let h_viewport = HorizontalViewport::new(content_width, widest);
        Self {
            size: (width, height),
            max_scroll: lines.len().saturating_sub(content_height),
            has_h_bar: horizontal && h_viewport.overflows() && content_width >= 3,
            h_viewport,
            lines,
            buttons,
            content_width,
            content_height,
        }
    }

    fn cached<'a>(
        layout: &'a mut Option<Self>,
        content: DocumentContent<'_>,
        area: Rect,
    ) -> &'a Self {
        let size = (area.width as usize, area.height as usize);
        if layout.as_ref().is_some_and(|layout| layout.size != size) {
            *layout = None;
        }
        layout.get_or_insert_with(|| Self::new(content, size.0, size.1))
    }

    fn content_area(&self, area: Rect) -> Rect {
        Rect::new(
            area.x,
            area.y,
            self.content_width as u16,
            self.content_height as u16,
        )
    }

    /// 纵向滚动条列：内容区右侧一列；底部横向滚动条存在时不占最后一行。
    fn vertical_bar_area(&self, area: Rect) -> Rect {
        Rect::new(
            area.right().saturating_sub(1),
            area.y,
            1,
            self.content_height as u16,
        )
    }

    /// 底部横向滚动条：内容区下方一行；`has_h_bar` 才存在。
    fn horizontal_bar(&self, area: Rect, h_scroll: usize) -> Option<(Rect, HorizontalBar)> {
        if !self.has_h_bar {
            return None;
        }
        let rect = Rect::new(
            area.x,
            area.y + self.content_height as u16,
            self.content_width as u16,
            1,
        );
        let bar = HorizontalBar::new(
            rect.width,
            self.h_viewport.viewport(),
            self.h_viewport.content(),
            self.h_viewport.clamp(h_scroll),
        )?;
        Some((rect, bar))
    }

    fn button_at(&self, area: Rect, scroll: usize, mouse: &MouseEvent) -> Option<usize> {
        let area = self.content_area(area);
        self.buttons.iter().position(|button| {
            let rect = button.screen_rect(area, scroll);
            (rect.top()..rect.bottom())
                .any(|row| hit_test(mouse.row, mouse.column, row, rect.left(), rect.right()))
        })
    }
}

/// 在同一次 segment 遍历里收集正文和文档坐标；Card 的列偏移已包含在渲染结果中。
fn layout_lines(source: &str, width: usize) -> (Vec<Line<'static>>, Vec<DocumentButton>) {
    let theme = dark_theme();
    let palette = theme.to_palette();
    let segments = parse_markdown(source, width, palette, theme.component.markdown.text);
    let mut lines = Vec::new();
    let mut buttons = Vec::new();
    let mut prev: Option<&MarkdownSegment> = None;
    for (index, segment) in segments.iter().enumerate() {
        // 与消息区的 segment 间距一致；行内图片不拆开段落。
        let gap = match (prev, segment) {
            (_, MarkdownSegment::Image(image)) if !image.standalone => false,
            (Some(MarkdownSegment::Image(image)), _) if !image.standalone => false,
            _ => {
                index > 0
                    && !lines
                        .last()
                        .is_some_and(|line: &Line| line.spans.is_empty())
            }
        };
        if gap {
            lines.push(Line::default());
        }
        match segment {
            MarkdownSegment::Text(segment_lines) => {
                lines.extend(segment_lines.iter().cloned());
            }
            MarkdownSegment::Table(data) => {
                let mut table_theme = TableTheme::from_palette(&palette);
                table_theme.row_style = Style::default();
                lines.extend(table_data_to_lines(data, &table_theme, width));
            }
            MarkdownSegment::Image(image) => lines.extend(image.lines.iter().cloned()),
            MarkdownSegment::Mermaid(segment) => lines.extend(segment.lines.iter().cloned()),
            MarkdownSegment::GenUi(segment) => {
                let rendered = genui::render(segment, width, &theme, None);
                let offset = lines.len();
                buttons.extend(rendered.buttons.into_iter().map(|button| DocumentButton {
                    rows: offset + button.line_start..offset + button.line_end,
                    x_start: button.x_start,
                    x_end: button.x_end,
                }));
                lines.extend(rendered.lines);
            }
        }
        prev = Some(segment);
    }
    (lines, buttons)
}

#[cfg(test)]
pub(crate) fn document_lines(source: &str, width: usize) -> Vec<Line<'static>> {
    layout_lines(source, width.max(1)).0
}

#[cfg(test)]
pub(crate) fn document_max_scroll(source: &str, width: usize, height: usize) -> usize {
    DocumentLayout::new(DocumentContent::Markdown(source), width, height).max_scroll
}

#[cfg(test)]
pub(crate) fn document_line_count(
    content: DocumentContent<'_>,
    width: usize,
    height: usize,
) -> usize {
    DocumentLayout::new(content, width.max(1), height.max(1))
        .lines
        .len()
}

#[cfg(test)]
pub(crate) fn document_h_scroll_max(
    content: DocumentContent<'_>,
    width: usize,
    height: usize,
) -> usize {
    DocumentLayout::new(content, width.max(1), height.max(1))
        .h_viewport
        .max_offset()
}

/// 文档变体共享的可变视图状态：竖直/水平偏移、按下态与缓存的排版。
/// `StoryState::Document` 和流式文档各持有一份，渲染与命中测试都从这里读写。
pub(crate) struct DocumentView<'a> {
    pub(crate) scroll: &'a mut usize,
    pub(crate) h_scroll: &'a mut usize,
    pub(crate) pressed_button: &'a mut Option<usize>,
    /// 横向滚动条 thumb 拖拽中：指针相对 thumb 起点的列偏移。
    pub(crate) h_drag: &'a mut Option<usize>,
    pub(crate) layout: &'a mut Option<DocumentLayout>,
}

/// 横向滚动条轨道内的列号（跳过左箭头）；越界列交给调用方 clamp。
fn track_cell(rect: Rect, column: u16) -> usize {
    usize::from(column.saturating_sub(rect.x.saturating_add(1)))
}

impl DocumentView<'_> {
    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer, content: DocumentContent<'_>) {
        let Self {
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        } = self;
        // 解构得到的是 `&mut &mut T`；再解一层，读写与普通自由函数一致。
        let scroll = &mut **scroll;
        let h_scroll = &mut **h_scroll;
        let pressed_button = &mut **pressed_button;
        let h_drag = &mut **h_drag;
        let layout = &mut **layout;
        if area.is_empty()
            || layout.as_ref().is_some_and(|layout| {
                layout.size != (usize::from(area.width), usize::from(area.height))
            })
        {
            *pressed_button = None;
            *h_drag = None;
        }
        if area.is_empty() {
            return;
        }
        let cached = DocumentLayout::cached(layout, content, area);
        *scroll = (*scroll).min(cached.max_scroll);
        *h_scroll = cached.h_viewport.clamp(*h_scroll);
        let h_offset = u16::try_from(*h_scroll).unwrap_or(u16::MAX);
        let content_area = cached.content_area(area);
        let mut lines = cached.lines[*scroll..].to_vec();
        if let Some(button) = (*pressed_button).and_then(|index| cached.buttons.get(index)) {
            let theme = dark_theme();
            let rect = button.screen_rect(content_area, *scroll);
            for row in rect.top()..rect.bottom() {
                // 水平偏移后 span 的屏幕列整体左移；Markdown 文档不横向滚动。
                let mut col = content_area.x.saturating_sub(h_offset);
                for span in &mut lines[(row - content_area.y) as usize].spans {
                    if hit_test(row, col, row, rect.left(), rect.right())
                        && span.style.add_modifier.contains(Modifier::REVERSED)
                        && let Some(fg) = span.style.fg
                    {
                        span.style = genui::render::pressed_button_style(fg, &theme);
                    }
                    col = col.saturating_add(span.width().min(u16::MAX as usize) as u16);
                }
            }
        }
        Paragraph::new(lines)
            .scroll((0, h_offset))
            .render(content_area, buf);
        if cached.max_scroll > 0 {
            let mut state = ScrollbarState::new(cached.lines.len())
                .position(*scroll)
                .viewport_content_length(cached.content_height);
            StatefulWidget::render(
                vertical_scrollbar(
                    Style::new().fg(MUTED).bg(MUTED),
                    muted_style().add_modifier(Modifier::BOLD),
                ),
                cached.vertical_bar_area(area),
                buf,
                &mut state,
            );
        }
        if let Some((rect, bar)) = cached.horizontal_bar(area, *h_scroll) {
            horizontal_scrollbar(
                rect,
                buf,
                Style::new().fg(MUTED).bg(MUTED),
                muted_style().add_modifier(Modifier::BOLD),
                &bar,
            );
        }
    }

    pub(super) fn handle_mouse(
        &mut self,
        area: Rect,
        mouse: &MouseEvent,
        content: DocumentContent<'_>,
    ) -> bool {
        let Self {
            scroll,
            h_scroll,
            pressed_button,
            h_drag,
            layout,
        } = self;
        // 解构得到的是 `&mut &mut T`；再解一层，读写与普通自由函数一致。
        let scroll = &mut **scroll;
        let h_scroll = &mut **h_scroll;
        let pressed_button = &mut **pressed_button;
        let h_drag = &mut **h_drag;
        let layout = &mut **layout;
        let inside = rect_contains(area, mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Up(MouseButton::Left) => {
                let dragged = h_drag.take().is_some();
                dragged || pressed_button.take().is_some()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                *h_drag = None;
                *pressed_button = None;
                if !inside {
                    return false;
                }
                let cached = DocumentLayout::cached(layout, content, area);
                *pressed_button = cached.button_at(area, (*scroll).min(cached.max_scroll), mouse);
                let Some((rect, bar)) = cached.horizontal_bar(area, *h_scroll) else {
                    return true;
                };
                if !rect_contains(rect, mouse.column, mouse.row) {
                    return true;
                }
                // 横向条优先于正文热区：按下即抓取；点在 thumb 外时让 thumb
                // 以指针为中心跳过去，拖动随后从那里继续。
                *pressed_button = None;
                let cell = track_cell(rect, mouse.column).min(bar.track - 1);
                match bar.grab(cell) {
                    Some(grab) => *h_drag = Some(grab),
                    None => {
                        let grab = bar.thumb_len / 2;
                        *h_drag = Some(grab);
                        *h_scroll = bar.offset_for(cell.saturating_sub(grab));
                    }
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(grab) = *h_drag else {
                    return false;
                };
                let cached = DocumentLayout::cached(layout, content, area);
                let Some((rect, bar)) = cached.horizontal_bar(area, *h_scroll) else {
                    *h_drag = None;
                    return false;
                };
                // 纵向越界不取消拖拽，只按列位置换算，和进度条 seek 一致。
                let cell = track_cell(rect, mouse.column);
                *h_scroll = bar.offset_for(cell.saturating_sub(grab));
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                *pressed_button = None;
                *h_drag = None;
                if !inside {
                    return false;
                }
                let cached = DocumentLayout::cached(layout, content, area);
                // Shift+滚轮在字符图上横向平移；没有横向溢出时退回纵向滚动。
                if mouse.modifiers.contains(KeyModifiers::SHIFT) && cached.h_viewport.overflows() {
                    *h_scroll = cached.h_viewport.pan(
                        *h_scroll,
                        if mouse.kind == MouseEventKind::ScrollUp {
                            -(WHEEL_COLS as i32)
                        } else {
                            WHEEL_COLS as i32
                        },
                    );
                    return true;
                }
                *scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    scroll.saturating_sub(WHEEL_ROWS)
                } else {
                    scroll.saturating_add(WHEEL_ROWS)
                }
                .min(cached.max_scroll);
                true
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                *pressed_button = None;
                *h_drag = None;
                if !inside {
                    return false;
                }
                let cached = DocumentLayout::cached(layout, content, area);
                // 一行高的视口放不下滚动条，但溢出列仍要能平移。
                if !cached.h_viewport.overflows() {
                    return false;
                }
                *h_scroll = cached.h_viewport.pan(
                    *h_scroll,
                    if mouse.kind == MouseEventKind::ScrollLeft {
                        -(WHEEL_COLS as i32)
                    } else {
                        WHEEL_COLS as i32
                    },
                );
                true
            }
            // 终端丢失左键 release 后只剩移动事件：清除悬空的按下态与 thumb 拖拽。
            MouseEventKind::Moved => {
                *pressed_button = None;
                *h_drag = None;
                false
            }
            _ => false,
        }
    }
}
