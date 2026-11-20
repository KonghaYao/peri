//! 输入区补全列表——`@` 提及与 `/` 命令共用的补全弹窗。
//!
//! [Why] 两处弹窗各自复制视口数学：提及把候选条数当可视行，命令另算
//! `scroll_start`；候选刷新或终端缩放后，绘制的行与点击命中的行可能来自
//! 不同窗口。这里以 [`ListViewport`] 为唯一来源：`window()` 产生绘制行，
//! `hit_layout()` 产生命中布局，两者共用同一次视口计算。

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_kit::crossterm::event::MouseEvent;

use crate::list::{ListMetrics, ListViewport};
use crate::panel_mouse::{ListLayout, hit_item};

/// `@` 提及的候选上限。搜索与排序留在宿主；弹窗只窗口化宿主给出的行。
pub const MAX_CANDIDATES: usize = 20;

/// 弹窗上下边框占用的行数（内联补全列表只有 TOP/BOTTOM）。
const BORDER_ROWS: u16 = 2;
const METRICS: ListMetrics = ListMetrics::new(0, 1, 0);

/// 补全列表的一行。`label` 由调用方格式化（含 `/` 前缀与列宽填充），
/// `detail` 是可选描述列；样式由调用方写在 `Line` 内。
#[derive(Debug, Clone)]
pub struct CompletionRow<'a> {
    pub label: Line<'a>,
    pub detail: Option<Line<'a>>,
}

impl<'a> CompletionRow<'a> {
    #[must_use]
    pub fn new(label: Line<'a>) -> Self {
        Self {
            label,
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: Line<'a>) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// 选中行与空态样式。未选中行的颜色随宿主写入的 `Line`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionStyles {
    pub border: Color,
    pub selected_label: Style,
    pub selected_detail: Style,
    pub empty: Style,
}

/// 内联补全弹窗。位置与高度由宿主布局决定，这里只负责可见窗口。
pub struct CompletionPopup<'a> {
    pub rows: Vec<CompletionRow<'a>>,
    pub selected: usize,
    /// 内联高度（含上下边框），由宿主主题决定。
    pub height: u16,
    pub title: Line<'a>,
    pub empty_text: Line<'a>,
    pub styles: CompletionStyles,
}

impl<'a> CompletionPopup<'a> {
    /// 与 `widget()` 同源的命中布局。
    #[must_use]
    pub fn hit_layout(&self) -> ListLayout {
        self.viewport().hit_layout()
    }

    /// 钳制后的选中下标（空列表为 0）。
    #[must_use]
    pub fn selected(&self) -> usize {
        crate::list_nav::clamp_selection(self.selected, self.rows.len())
    }

    /// 鼠标命中 → 全量列表下标。调用方需传入弹窗自身区域（含上下边框）。
    #[must_use]
    pub fn hit(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        hit_item(mouse, area, self.hit_layout())
    }

    /// 由同一视口生成可见行与边框；宿主直接用 `Paragraph` 渲染。
    #[must_use]
    pub fn widget(&self) -> Paragraph<'a> {
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::new().fg(self.styles.border))
            .title_top(self.title.clone());
        let selected = self.selected();
        let lines: Vec<Line<'a>> = if self.rows.is_empty() {
            vec![self.empty_text.clone().patch_style(self.styles.empty)]
        } else {
            self.viewport()
                .window(&self.rows)
                .map(|(index, row)| self.row_line(index, row, selected))
                .collect()
        };
        Paragraph::new(Text::from(lines)).block(block)
    }

    fn viewport(&self) -> ListViewport {
        ListViewport::for_content_rows(
            self.height.saturating_sub(BORDER_ROWS),
            METRICS,
            self.rows.len(),
            self.selected,
        )
    }

    fn row_line(&self, index: usize, row: &CompletionRow<'a>, selected: usize) -> Line<'a> {
        let is_selected = index == selected;
        let marker_style = if is_selected {
            self.styles.selected_label
        } else {
            Style::default()
        };
        let label_patch = if is_selected {
            self.styles.selected_label
        } else {
            Style::default()
        };
        let mut spans = Vec::with_capacity(row.label.spans.len() + 2);
        spans.push(Span::styled(
            if is_selected { "> " } else { "  " },
            marker_style,
        ));
        spans.extend(
            row.label
                .spans
                .iter()
                .cloned()
                .map(|span| span.patch_style(row.label.style.patch(label_patch))),
        );
        if let Some(detail) = &row.detail {
            let detail_patch = if is_selected {
                self.styles.selected_detail
            } else {
                Style::default()
            };
            let detail_style = detail.style.patch(detail_patch);
            spans.push(Span::styled("  ", detail_style));
            spans.extend(
                detail
                    .spans
                    .iter()
                    .cloned()
                    .map(|span| span.patch_style(detail_style)),
            );
        }
        Line::from(spans)
    }
}

#[cfg(test)]
#[path = "completion_test.rs"]
mod tests;
