//! 图片绘制的共享几何：居中放置 + 视口内屏幕矩形。
//!
//! 各 surface 的布局/滚动/命中语义留在调用方；这里只处理「给定区域与图片格数，
//! 往哪里画」——composer 预览、转录区/工具卡的 draw hook、@image 浮层共用。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::kit::image_preview::InlineImage;

/// 水平居中偏移（`cols` 大于区域宽度时为 0）。
#[must_use]
pub(crate) fn centered_x(area_width: u16, cols: u16) -> u16 {
    area_width.saturating_sub(cols) / 2
}

/// 在区域内水平居中、顶部对齐的矩形；图片超出区域时返回 `None`（调用方降级）。
#[must_use]
pub(crate) fn centered_rect(area: Rect, cols: u16, rows: u16) -> Option<Rect> {
    if cols == 0 || rows == 0 || cols > area.width || rows > area.height {
        return None;
    }
    Some(Rect::new(
        area.x.saturating_add(centered_x(area.width, cols)),
        area.y,
        cols,
        rows,
    ))
}

/// 绘制已编码图片；面积必须不小于编码尺寸（不足时调用方应先裁剪）。
pub(crate) fn draw(buf: &mut Buffer, rect: Rect, image: &InlineImage) {
    image.render(rect, buf);
}

/// 把槽内视觉行折算成屏幕矩形；越界（区域缩小/滚动偏移变化）返回 `None`。
#[must_use]
pub(crate) fn screen_rect(
    area: Rect,
    vis_row: usize,
    scroll_y: usize,
    x: u16,
    cols: u16,
    rows: u16,
) -> Option<Rect> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let row = i64::from(area.y) + vis_row as i64 - scroll_y as i64;
    let top = i64::from(area.y);
    let bottom = i64::from(area.bottom());
    if row < top || row + i64::from(rows) > bottom {
        return None;
    }
    let x = i64::from(area.x) + i64::from(x);
    if x + i64::from(cols) > i64::from(area.right()) {
        return None;
    }
    Some(Rect::new(
        u16::try_from(x).ok()?,
        u16::try_from(row).ok()?,
        cols,
        rows,
    ))
}
