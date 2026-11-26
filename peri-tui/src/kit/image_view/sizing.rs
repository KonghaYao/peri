//! 图片展示的纯几何：像素尺寸 + 单元格尺寸 + 区域预算 → 展示格数。
//!
//! 所有 surface（转录区、用户气泡、composer 预览、@image 浮层、工具卡）共用同一
//! 公式，差异只由 [`Budget`]（区域约束）与 [`Fill`]（是否允许放大）表达；布局、
//! 滚动与命中仍留在各 surface。
//!
//! 公式（等比铺满）：
//!
//! ```text
//! avail_w = cols * cell_w          avail_h = max_rows * cell_h
//! s       = min(avail_w/iw, avail_h/ih, fill.max_scale())
//! tw = floor(iw * s)               th = floor(ih * s)
//! cell_cols = min(ceil(tw/cell_w), cols)
//! cell_rows = min(ceil(th/cell_h), max_rows)
//! x_offset  = (cols - cell_cols) / 2
//! ```
//!
//! - **像素 floor**：Sixel 直接按像素落屏，向上取整会让编码像素越过预留区；
//! - **单元格 ceil**：区域预算先触底时保证受限轴贴边
//!   （`cell_cols == cols || cell_rows == max_rows`），否则 `Resize::Fit` 的
//!   「不放大」会让小图永远停在自然尺寸、右侧/下方留白。**放大上限也是约束**：
//!   当 `fill.max_scale()` 先于 `avail_w/iw` 与 `avail_h/ih` 触底（小图被 2× 上限
//!   截住）时，两轴都可能留白，上述贴边等式不成立；
//! - **居中**：竖图高度贴边后宽度居中，横图宽度贴边后高度按比例。

/// 区域预算（单元格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Budget {
    /// 可用列数。
    pub cols: u16,
    /// 最大行数。
    pub max_rows: u16,
}

impl Budget {
    #[must_use]
    pub fn new(cols: u16, max_rows: u16) -> Self {
        Self { cols, max_rows }
    }

    /// 任一维为零时没有可绘制区域。
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.cols == 0 || self.max_rows == 0
    }
}

/// 铺满策略：限制放大倍数，避免小图被拉伸成马赛克。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Fill {
    /// 不放大：大图缩小铺满，小图按自然单元格尺寸显示。
    NoUpscale,
    /// 最多放大到自然单元格尺寸的 2 倍（终端图片的常用上限）。
    #[default]
    Upscale2x,
}

/// 放大上限：超过后像素糊化比留白更影响可读性。
pub const UPSCALE_CAP: f32 = 2.0;

impl Fill {
    #[must_use]
    pub fn max_scale(self) -> f32 {
        match self {
            Self::NoUpscale => 1.0,
            Self::Upscale2x => UPSCALE_CAP,
        }
    }
}

/// 图片在预算区域内的展示几何。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cells {
    /// 展示列数（≤ 预算列数）。
    pub cols: u16,
    /// 展示行数（≤ 预算行数）。
    pub rows: u16,
    /// 水平居中偏移（相对预算区域左边）。
    pub x_offset: u16,
}

/// 等比铺满计算。参数非法（零尺寸、非有限比例）返回 `None`。
#[must_use]
pub fn fit(iw: u32, ih: u32, cell: (u16, u16), budget: Budget, fill: Fill) -> Option<Cells> {
    let (cell_w, cell_h) = cell;
    if iw == 0 || ih == 0 || cell_w == 0 || cell_h == 0 || budget.is_empty() {
        return None;
    }
    let avail_w = f32::from(budget.cols) * f32::from(cell_w);
    let avail_h = f32::from(budget.max_rows) * f32::from(cell_h);
    let scale = (avail_w / iw as f32)
        .min(avail_h / ih as f32)
        .min(fill.max_scale());
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    // floor：像素不越界（Sixel 按像素落屏）；至少保留 1 像素，避免极小图消失。
    let target_w = (iw as f32 * scale).floor().max(1.0) as u32;
    let target_h = (ih as f32 * scale).floor().max(1.0) as u32;
    let target_w = target_w.min(avail_w as u32);
    let target_h = target_h.min(avail_h as u32);
    // ceil：受限轴贴边，最多向上取整 1 格。
    let cols = u16::try_from(target_w.div_ceil(u32::from(cell_w)))
        .unwrap_or(u16::MAX)
        .min(budget.cols)
        .max(1);
    let rows = u16::try_from(target_h.div_ceil(u32::from(cell_h)))
        .unwrap_or(u16::MAX)
        .min(budget.max_rows)
        .max(1);
    Some(Cells {
        cols,
        rows,
        x_offset: budget.cols.saturating_sub(cols) / 2,
    })
}

#[cfg(test)]
#[path = "sizing_test.rs"]
mod tests;
