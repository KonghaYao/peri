use std::ops::Range;

use peri_theme::{bridge::ThemeDefinitionExt, builtin::dark_theme};
use peri_tui_components::button::hit_test;
use peri_tui_components::genui;
use peri_tui_components::markdown::{MarkdownSegment, parse_markdown, table_data_to_lines};
use peri_tui_components::scrollbar::vertical_scrollbar;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, ScrollbarState, StatefulWidget, Widget};
use ratatui_kit::{ComponentTheme, components::TableTheme};

use super::{MUTED, WHEEL_ROWS, mermaid, muted_style, rect_contains};

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
    max_scroll: usize,
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
        let (mut lines, mut buttons) = layout(content_width);
        // 保留原有滚动条列；正文与热区必须都使用最终的内容宽度。
        if lines.len() > height && content_width > 1 {
            content_width -= 1;
            (lines, buttons) = layout(content_width);
        }
        Self {
            size: (width, height),
            max_scroll: lines.len().saturating_sub(height),
            lines,
            buttons,
            content_width,
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
        Rect::new(area.x, area.y, self.content_width as u16, area.height)
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

pub(super) fn render(
    area: Rect,
    buf: &mut Buffer,
    content: DocumentContent<'_>,
    scroll: &mut usize,
    pressed_button: &mut Option<usize>,
    layout: &mut Option<DocumentLayout>,
) {
    if area.is_empty()
        || layout.as_ref().is_some_and(|layout| {
            layout.size != (usize::from(area.width), usize::from(area.height))
        })
    {
        *pressed_button = None;
    }
    if area.is_empty() {
        return;
    }
    let layout = DocumentLayout::cached(layout, content, area);
    *scroll = (*scroll).min(layout.max_scroll);
    let content_area = layout.content_area(area);
    let mut lines = layout.lines[*scroll..].to_vec();
    if let Some(button) = pressed_button.and_then(|index| layout.buttons.get(index)) {
        let theme = dark_theme();
        let rect = button.screen_rect(content_area, *scroll);
        for row in rect.top()..rect.bottom() {
            let mut col = content_area.x;
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
    Paragraph::new(lines).render(content_area, buf);
    if layout.max_scroll > 0 {
        let bar_area = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        let mut state = ScrollbarState::new(layout.lines.len())
            .position(*scroll)
            .viewport_content_length(area.height as usize);
        StatefulWidget::render(
            vertical_scrollbar(
                Style::new().fg(MUTED).bg(MUTED),
                muted_style().add_modifier(Modifier::BOLD),
            ),
            bar_area,
            buf,
            &mut state,
        );
    }
}

pub(super) fn handle_mouse(
    area: Rect,
    mouse: &MouseEvent,
    content: DocumentContent<'_>,
    scroll: &mut usize,
    pressed_button: &mut Option<usize>,
    layout: &mut Option<DocumentLayout>,
) -> bool {
    let inside = rect_contains(area, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Up(MouseButton::Left) => pressed_button.take().is_some(),
        MouseEventKind::Down(MouseButton::Left) => {
            *pressed_button = if inside {
                let layout = DocumentLayout::cached(layout, content, area);
                layout.button_at(area, (*scroll).min(layout.max_scroll), mouse)
            } else {
                None
            };
            inside
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            *pressed_button = None;
            if inside {
                let max = DocumentLayout::cached(layout, content, area).max_scroll;
                *scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    scroll.saturating_sub(WHEEL_ROWS)
                } else {
                    scroll.saturating_add(WHEEL_ROWS)
                }
                .min(max);
            }
            inside
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            *pressed_button = None;
            false
        }
        // 终端丢失左键 release 后只剩移动事件：清除悬空的按下态。
        MouseEventKind::Moved => {
            *pressed_button = None;
            false
        }
        // 拖拽期间保留按下态，等 Up 统一收尾。
        MouseEventKind::Drag(MouseButton::Left) => false,
        _ => false,
    }
}
