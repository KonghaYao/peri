//! 弹窗外框——确认与下载弹窗共用的上下边框、标题与内区。
//!
//! [Why] 两个弹窗此前各自写 `Block::borders(TOP | BOTTOM)`、居中标題和点击范围；
//! 外壳与命中几何重复，容易在一边改了边框或内边距后另一边仍按旧尺寸命中。
//! 这里只画边框和标题，**不清屏**：宿主在既有位置（`Positioned(clear)`）只清
//! 一次，确认动作、下载状态和弹窗外点击仍由宿主决定。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Widget};
use ratatui_kit::crossterm::event::MouseEvent;

/// 上下边框占用的行数。
pub const BORDER_ROWS: u16 = 2;

/// 外框边界命中结果。只有上下边框，左右列不构成边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayHit {
    /// 上边框或下边框行。
    Border,
    /// 上下边框之间的内区。
    Inside,
    /// 外框之外（含零尺寸的首帧）。
    Outside,
}

/// 弹窗外壳：居中的标题行与上下边框。尺寸与位置由宿主决定。
pub struct OverlayFrame<'a> {
    title: Line<'a>,
    border: Style,
    title_style: Style,
}

impl<'a> OverlayFrame<'a> {
    #[must_use]
    pub fn new(title: impl Into<Line<'a>>) -> Self {
        Self {
            title: title.into(),
            border: Style::default(),
            title_style: Style::default(),
        }
    }

    #[must_use]
    pub fn border(mut self, style: Style) -> Self {
        self.border = style;
        self
    }

    #[must_use]
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    /// 去掉上下边框后的内容区（宽高饱和，边框放不下时高度为 0）。
    #[must_use]
    pub fn inner(&self, area: Rect) -> Rect {
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(BORDER_ROWS),
        )
    }

    /// 上下边框 + 居中标题。宿主可直接作为 `Paragraph` 的 block 复用。
    #[must_use]
    pub fn widget(&self) -> Block<'a> {
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(self.border)
            .title_top(self.title.clone().patch_style(self.title_style).centered())
    }

    /// 只画外壳；内区留给宿主按 `inner()` 绘制。
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.widget().render(area, buf);
    }
}

/// 外框边界命中。鼠标事件类型不参与判定——滚轮同样按弹窗内外分流。
#[must_use]
pub fn hit(mouse: &MouseEvent, area: Rect) -> OverlayHit {
    if area.width == 0
        || area.height == 0
        || mouse.column < area.x
        || mouse.column >= area.right()
        || mouse.row < area.y
        || mouse.row >= area.bottom()
    {
        return OverlayHit::Outside;
    }
    if mouse.row == area.y || mouse.row == area.bottom().saturating_sub(1) {
        OverlayHit::Border
    } else {
        OverlayHit::Inside
    }
}

#[cfg(test)]
#[path = "overlay_test.rs"]
mod tests;
