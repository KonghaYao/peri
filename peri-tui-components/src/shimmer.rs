//! 纯函数文字扫光——宿主给文本、相位与颜色，组件只回答每字符这一帧的样式。
//!
//! [Why] 折叠组状态标签沿用 Codex shimmer 的形状（一条余弦光带按固定周期扫过
//! 整词），但相位由宿主注入（帧序号 × 帧间隔），组件不读时钟、终端能力或主题
//! atom：固定输入即可在单测与 storybook 里复现同一帧。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// 一次完整扫光周期——只决定相位回绕，与宿主帧率无关。
pub const SWEEP_MS: u64 = 2_000;

/// 光带半宽与文本两侧留白（字符位）——对齐 Codex `shimmer.rs` 的扫光形状。
const BAND_HALF_WIDTH: f32 = 5.0;
const PADDING: usize = 10;

/// 扫光样式输入：基色、高光色、truecolor 能力与 reduced-motion 开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShimmerStyle {
    pub base: Color,
    pub highlight: Color,
    /// 宿主探测的 24 位颜色能力；false 走 DIM / 普通 / BOLD 强度分级。
    pub truecolor: bool,
    /// false 时返回单个静态 `base` 色 span（标签保留，不运动）。
    pub animated: bool,
}

/// 把 `text` 拆成逐字符 span，按 `phase_ms` 的光带位置着色。
///
/// 空文本返回空 span；`animated == false` 返回整词单 span。
#[must_use]
pub fn shimmer_spans(text: &str, phase_ms: u64, style: ShimmerStyle) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    if !style.animated {
        return vec![Span::styled(
            text.to_string(),
            Style::default().fg(style.base),
        )];
    }
    let period = chars.len() + PADDING * 2;
    let sweep = (phase_ms % SWEEP_MS) as f32 / SWEEP_MS as f32;
    let pos = sweep * period as f32;
    // 非 RGB 主题色无法插值：退回强度分级，避免整词只剩 BOLD 看不出运动。
    let blend_colors = match (rgb(style.base), rgb(style.highlight)) {
        (Some(base), Some(highlight)) if style.truecolor => Some((base, highlight)),
        _ => None,
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        let dist = (i as f32 + PADDING as f32 - pos).abs();
        let t = if dist <= BAND_HALF_WIDTH {
            0.5 * (1.0 + (std::f32::consts::PI * dist / BAND_HALF_WIDTH).cos())
        } else {
            0.0
        };
        let span_style = match blend_colors {
            Some((base, highlight)) => {
                let (r, g, b) = blend(highlight, base, t * 0.9);
                Style::default()
                    .fg(Color::Rgb(r, g, b))
                    .add_modifier(Modifier::BOLD)
            }
            None => intensity_style(style.base, t),
        };
        spans.push(Span::styled(ch.to_string(), span_style));
    }
    spans
}

/// 非 truecolor 的强度分级：远光带变暗、近光带加粗，保证无 RGB 时仍可辨识。
fn intensity_style(base: Color, intensity: f32) -> Style {
    let style = Style::default().fg(base);
    if intensity < 0.2 {
        style.add_modifier(Modifier::DIM)
    } else if intensity < 0.6 {
        style
    } else {
        style.add_modifier(Modifier::BOLD)
    }
}

fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// `alpha` 为高光权重：1.0 全高光，0.0 全基色。
fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let mix = |fg: u8, bg: u8| (fg as f32 * alpha + bg as f32 * (1.0 - alpha)) as u8;
    (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
}

#[cfg(test)]
#[path = "shimmer_test.rs"]
mod tests;
