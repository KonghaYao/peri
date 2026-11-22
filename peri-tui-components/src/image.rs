//! 纯值图片渲染：RGBA 采样、半块降级与附件编号。
//!
//! 不读终端、不编码像素协议、不持有全局状态；协议选择与编码留在宿主
//! （`peri-tui/src/kit/{terminal_caps,image_preview}.rs`）。半块是任何终端
//! 都能显示的最后降级层：`▀` 上色、`▄` 下色、`█` 同色、空格透明。

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// 半块渲染视为不透明的 alpha 阈值。
///
/// 终端单元格没有 alpha 通道，也不保证能拿到主题背景做混合；低于阈值的
/// 像素直接当透明，避免把半透明边缘画成黑边。
const ALPHA_OPAQUE_MIN: u8 = 128;

/// 已校验尺寸的 RGBA 图片。
///
/// 只能经 [`ImageData::new`] 构造，保证 `rgba.len() == width * height * 4`；
/// 零宽或零高图片没有可渲染内容，构造返回 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl ImageData {
    /// 校验尺寸与缓冲区长度后构造图片。
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let expected = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        (rgba.len() == expected).then_some(Self {
            width,
            height,
            rgba,
        })
    }

    /// 图片像素宽度。
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 图片像素高度。
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 逐行 RGBA 字节（长度恒为 `width * height * 4`）。
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    fn rgba_at(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[offset],
            self.rgba[offset + 1],
            self.rgba[offset + 2],
            self.rgba[offset + 3],
        ]
    }
}

/// 等比适配后的单元格尺寸与缩放目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageFit {
    /// 目标列数（至少 1，且不超过 bounds 列数）。
    pub columns: u16,
    /// 目标行数（至少 1，且不超过 bounds 行数）。
    pub rows: u16,
    /// 落在这些单元格内的像素尺寸；像素协议可据此 resize。
    pub pixels: (u32, u32),
}

/// 把图片像素尺寸等比适配进可用单元格。
///
/// - `image_px`：图片原始像素尺寸；
/// - `cell_px`：单个终端单元格的像素尺寸；半块渲染传 `(1, 2)`（纵向两像素
///   合成一个单元格）；
/// - `bounds`：可用单元格 `(列, 行)`。
///
/// 返回的目标列/行不会超过 bounds，且至少为 1。宽高比只由像素尺寸和
/// `cell_px` 决定，不假设单元格是正方形；极小目标按 1 像素/1 单元格兜底，
/// 此时比例只能近似。任一输入为零返回 `None`。
#[must_use]
pub fn fit(image_px: (u32, u32), cell_px: (u16, u16), bounds: (u16, u16)) -> Option<ImageFit> {
    let (image_width, image_height) = (u64::from(image_px.0), u64::from(image_px.1));
    let (cell_width, cell_height) = (u64::from(cell_px.0), u64::from(cell_px.1));
    let (max_columns, max_rows) = (u64::from(bounds.0), u64::from(bounds.1));
    if image_width == 0
        || image_height == 0
        || cell_width == 0
        || cell_height == 0
        || max_columns == 0
        || max_rows == 0
    {
        return None;
    }

    let available_width = max_columns * cell_width;
    let available_height = max_rows * cell_height;
    // 交叉相乘比较两条缩放上限，避免浮点误差；u128 覆盖 u32 × u16² 的中间值。
    let width_limited = u128::from(available_width) * u128::from(image_height)
        <= u128::from(available_height) * u128::from(image_width);
    let (pixel_width, pixel_height) = if width_limited {
        let scaled_height =
            u128::from(image_height) * u128::from(available_width) / u128::from(image_width);
        (available_width, u64::try_from(scaled_height).ok()?.max(1))
    } else {
        let scaled_width =
            u128::from(image_width) * u128::from(available_height) / u128::from(image_height);
        (u64::try_from(scaled_width).ok()?.max(1), available_height)
    };

    Some(ImageFit {
        columns: u16::try_from(pixel_width.div_ceil(cell_width).clamp(1, max_columns)).ok()?,
        rows: u16::try_from(pixel_height.div_ceil(cell_height).clamp(1, max_rows)).ok()?,
        pixels: (
            u32::try_from(pixel_width).ok()?,
            u32::try_from(pixel_height).ok()?,
        ),
    })
}

/// 把图片采样为半块字符行。
///
/// `area` 是目标单元格 `(列, 行)`：每行恒为 `area.0` 个窄字符，纵向每行
/// 取上、下两个采样点。`style` 提供基础样式（含透明像素的背景/前景）；不透明
/// 像素覆盖前景，上半/下半都有颜色时用 `▀` 并让下半色落到背景。
///
/// 采样按单元格中心最近邻取值；图片小于目标时放大，大于目标时抽样，不分配
/// 中间缩放图。零宽或零高返回空 Vec。
#[must_use]
pub fn halfblock_lines(image: &ImageData, area: (u16, u16), style: Style) -> Vec<Line<'static>> {
    let (columns, rows) = (u32::from(area.0), u32::from(area.1));
    if columns == 0 || rows == 0 {
        return Vec::new();
    }

    let sample_rows = rows * 2;
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let top_y = sample_index(row * 2, sample_rows, image.height);
        let bottom_y = sample_index(row * 2 + 1, sample_rows, image.height);
        let mut spans = Vec::with_capacity(columns as usize);
        for column in 0..columns {
            let x = sample_index(column, columns, image.width);
            let top = opaque_color(image.rgba_at(x, top_y));
            let bottom = opaque_color(image.rgba_at(x, bottom_y));
            spans.push(halfblock_span(top, bottom, style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// 多图附件标签（1-based），如 `[Image #2]`。
#[must_use]
pub fn attachment_label(index: usize) -> String {
    format!("[Image #{index}]")
}

/// 把第 `sample` 个采样点映射到源图坐标（按采样区间中心取整）。
fn sample_index(sample: u32, sample_count: u32, source_len: u32) -> u32 {
    let numerator = (u64::from(sample) * 2 + 1) * u64::from(source_len);
    let denominator = u64::from(sample_count) * 2;
    u32::try_from(numerator / denominator)
        .unwrap_or(source_len - 1)
        .min(source_len - 1)
}

fn opaque_color(pixel: [u8; 4]) -> Option<Color> {
    (pixel[3] >= ALPHA_OPAQUE_MIN).then(|| Color::Rgb(pixel[0], pixel[1], pixel[2]))
}

fn halfblock_span(top: Option<Color>, bottom: Option<Color>, base: Style) -> Span<'static> {
    match (top, bottom) {
        (Some(top), Some(bottom)) if top == bottom => Span::styled("█", base.fg(top).bg(top)),
        (Some(top), Some(bottom)) => Span::styled("▀", base.fg(top).bg(bottom)),
        (Some(top), None) => Span::styled("▀", base.fg(top)),
        (None, Some(bottom)) => Span::styled("▄", base.fg(bottom)),
        (None, None) => Span::styled(" ", base),
    }
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
