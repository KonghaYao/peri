//! 终端单元格 → 网关 viewport CSS 坐标的映射（面板点击，A2）。
//!
//! 鼠标点击必须来自真实终端事件（面板 `AreaTracker`/绘制矩形命中），经本
//! 模块换算成网关 `/click` 所需的 CSS 视口坐标；不得由命令或代码直接造坐标。
//!
//! 帧的 `width/height` 是截图像素尺寸，`viewportWidth/viewportHeight` 是页面
//! CSS 视口尺寸——两者比例一致，App 帧覆盖整个视口，因此映射按「图像内相对
//! 比例 × 视口尺寸」计算，并对等比适配后的居中黑边做扣除。

use ratatui_kit::ratatui::layout::Rect;

/// 帧几何（来自 `frame` 事件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameGeometry {
    pub width: u32,
    pub height: u32,
    pub viewport_width: u32,
    pub viewport_height: u32,
}

/// 终端单元格像素尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

impl CellSize {
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// 等比适配后的图像在面板中的单元格矩形（居中）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageFit {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// 视口 CSS 坐标（网关 `/click` 直接消费）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportPoint {
    pub x: f64,
    pub y: f64,
}

/// 按纵横比把帧放进 `area`：返回居中的图像单元格矩形。
///
/// 任一输入为 0 返回 `None`；最小 1×1（避免除零）。
#[must_use]
pub fn fit_image(geometry: &FrameGeometry, area: Rect, cell: CellSize) -> Option<ImageFit> {
    if !cell.is_valid()
        || area.width == 0
        || area.height == 0
        || geometry.width == 0
        || geometry.height == 0
    {
        return None;
    }
    let available_w = f64::from(area.width) * f64::from(cell.width);
    let available_h = f64::from(area.height) * f64::from(cell.height);
    let aspect = f64::from(geometry.width) / f64::from(geometry.height);

    // 先试满宽；放不下再按满高收窄。取整后至少 1 行/列。
    let width_limited_rows = available_w / aspect / f64::from(cell.height);
    let (cols, rows) = if width_limited_rows <= f64::from(area.height) {
        (
            area.width,
            (width_limited_rows.floor() as u16).clamp(1, area.height),
        )
    } else {
        let height_limited_cols = available_h * aspect / f64::from(cell.width);
        (
            (height_limited_cols.floor() as u16).clamp(1, area.width),
            area.height,
        )
    };

    Some(ImageFit {
        x: area.x + area.width.saturating_sub(cols) / 2,
        y: area.y + area.height.saturating_sub(rows) / 2,
        cols,
        rows,
    })
}

/// 终端单元格 → 视口 CSS 坐标；落在图像外返回 `None`。
///
/// 使用单元格中心点（`+0.5`）避免边缘采样落在 0/1 边界。
#[must_use]
pub fn cell_to_viewport(
    geometry: &FrameGeometry,
    fit: &ImageFit,
    column: u16,
    row: u16,
) -> Option<ViewportPoint> {
    if geometry.viewport_width == 0 || geometry.viewport_height == 0 {
        return None;
    }
    if fit.cols == 0 || fit.rows == 0 {
        return None;
    }
    if column < fit.x || row < fit.y {
        return None;
    }
    let local_x = column - fit.x;
    let local_y = row - fit.y;
    if local_x >= fit.cols || local_y >= fit.rows {
        return None;
    }
    let fraction_x = (f64::from(local_x) + 0.5) / f64::from(fit.cols);
    let fraction_y = (f64::from(local_y) + 0.5) / f64::from(fit.rows);
    Some(ViewportPoint {
        x: fraction_x * f64::from(geometry.viewport_width),
        y: fraction_y * f64::from(geometry.viewport_height),
    })
}

/// 便捷入口：一次完成适配 + 映射（无预计算绘制矩形时使用）。
#[must_use]
pub fn terminal_cell_to_viewport(
    geometry: &FrameGeometry,
    area: Rect,
    cell: CellSize,
    column: u16,
    row: u16,
) -> Option<ViewportPoint> {
    let fit = fit_image(geometry, area, cell)?;
    cell_to_viewport(geometry, &fit, column, row)
}

#[cfg(test)]
#[path = "mapping_test.rs"]
mod tests;
