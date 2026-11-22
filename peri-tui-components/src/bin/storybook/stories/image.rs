//! Image 故事：内存生成的 RGBA 渐变，不读文件、不探测终端协议。
//!
//! 预览只走组件的半块降级路径（`▀`/`▄`/`█`/空格），四角保留透明像素，
//! 用来检查透明处理与列宽预算。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Paragraph, Widget};

use peri_tui_components::image::{ImageData, fit, halfblock_lines};

use super::{BG, StoryState, Variant};

const IMAGE_WIDTH: u32 = 96;
const IMAGE_HEIGHT: u32 = 64;
const TRANSPARENT_CORNER: u32 = 8;

pub(crate) struct ImageState {
    image: ImageData,
}

fn gradient_initial() -> StoryState {
    StoryState::Image(ImageState {
        image: gradient_image(),
    })
}

pub(crate) const VARIANTS: &[Variant] = &[Variant {
    key: "halfblocks",
    note: "RGBA gradient sampled into upper/lower half blocks; transparent corners stay blank.",
    initial: gradient_initial,
}];

/// 双色渐变 + 透明角：R 随 x、G 随 y、B 随对角线，四角 alpha=0。
fn gradient_image() -> ImageData {
    let mut rgba = Vec::with_capacity((IMAGE_WIDTH * IMAGE_HEIGHT * 4) as usize);
    for y in 0..IMAGE_HEIGHT {
        for x in 0..IMAGE_WIDTH {
            let red = (x * 255 / (IMAGE_WIDTH - 1)) as u8;
            let green = (y * 255 / (IMAGE_HEIGHT - 1)) as u8;
            let blue = ((x + y) * 255 / (IMAGE_WIDTH + IMAGE_HEIGHT - 2)) as u8;
            let transparent = (x < TRANSPARENT_CORNER && y < TRANSPARENT_CORNER)
                || (x >= IMAGE_WIDTH - TRANSPARENT_CORNER
                    && y >= IMAGE_HEIGHT - TRANSPARENT_CORNER);
            rgba.extend_from_slice(&[red, green, blue, if transparent { 0 } else { 255 }]);
        }
    }
    ImageData::new(IMAGE_WIDTH, IMAGE_HEIGHT, rgba)
        .expect("gradient dimensions match the generated RGBA buffer")
}

pub(crate) fn render(state: &ImageState, area: Rect, buf: &mut Buffer) {
    // 半块单元格是 1x2 像素：先等比 fit，再居中，避免预览区拉伸图片。
    let Some(fitted) = fit(
        (state.image.width(), state.image.height()),
        (1, 2),
        (area.width, area.height),
    ) else {
        return;
    };
    let width = fitted.columns.min(area.width);
    let height = fitted.rows.min(area.height);
    let target = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let lines = halfblock_lines(&state.image, (width, height), Style::new().bg(BG));
    Paragraph::new(lines).render(target, buf);
}
