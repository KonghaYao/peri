//! `shimmer_spans` 的相位、强度分级与降级回归。
//!
//! 关键约定：相位由宿主注入；组件只做 `phase_ms % SWEEP_MS` 的光带定位，
//! 不读时钟、终端能力或主题 atom。

use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

use super::{SWEEP_MS, ShimmerStyle, shimmer_spans};

const BASE: Color = Color::Rgb(10, 20, 30);
const HIGHLIGHT: Color = Color::Rgb(200, 210, 220);

fn style(truecolor: bool) -> ShimmerStyle {
    ShimmerStyle {
        base: BASE,
        highlight: HIGHLIGHT,
        truecolor,
        animated: true,
    }
}

fn text_of(spans: &[ratatui::text::Span<'static>]) -> String {
    spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn modifiers(spans: &[ratatui::text::Span<'static>]) -> Vec<Modifier> {
    spans.iter().map(|span| span.style.add_modifier).collect()
}

fn red(span: &ratatui::text::Span<'static>) -> Option<u8> {
    match span.style.fg {
        Some(Color::Rgb(r, _, _)) => Some(r),
        _ => None,
    }
}

#[test]
fn empty_text_has_no_spans() {
    assert!(shimmer_spans("", 0, style(true)).is_empty());
    assert!(shimmer_spans("", 700, style(false)).is_empty());
    assert!(
        shimmer_spans(
            "",
            0,
            ShimmerStyle {
                animated: false,
                ..style(true)
            }
        )
        .is_empty()
    );
}

#[test]
fn different_phases_restyle_every_character() {
    let first = shimmer_spans("Working", 0, style(true));
    let second = shimmer_spans("Working", 500, style(true));
    assert_eq!(first.len(), "Working".chars().count());
    assert_eq!(second.len(), first.len());
    let first_styles: Vec<_> = first
        .iter()
        .map(|span| (span.style.fg, span.style.add_modifier))
        .collect();
    let second_styles: Vec<_> = second
        .iter()
        .map(|span| (span.style.fg, span.style.add_modifier))
        .collect();
    assert_ne!(first_styles, second_styles, "同一文本在不同相位必须换样式");
}

#[test]
fn phase_wraps_after_one_sweep() {
    // 相位只对 SWEEP_MS 取模：整周期后必须回到同一帧。
    let start = shimmer_spans("Working", 13, style(true));
    let wrapped = shimmer_spans("Working", 13 + SWEEP_MS * 3, style(true));
    assert_eq!(
        start.iter().map(|span| span.style.fg).collect::<Vec<_>>(),
        wrapped.iter().map(|span| span.style.fg).collect::<Vec<_>>()
    );
}

#[test]
fn band_centre_is_highlight_and_far_characters_stay_base() {
    // "ABCDEFGHIJ" 的 period = 30；phase 667ms → 光带中心落在首字符（i_pos = 10）。
    let spans = shimmer_spans("ABCDEFGHIJ", 667, style(true));
    assert_eq!(text_of(&spans), "ABCDEFGHIJ");
    let centre = red(&spans[0]).expect("truecolor 输出 RGB");
    let far = red(&spans[9]).expect("truecolor 输出 RGB");
    assert!(centre >= 170, "光带中心应接近高光色 200，实际 {centre}");
    assert_eq!(Some(far), Some(10), "远端字符保持基色");
    assert_eq!(modifiers(&spans), vec![Modifier::BOLD; 10]);
}

#[test]
fn non_truecolor_uses_three_intensity_levels() {
    let spans = shimmer_spans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", 1000, style(false));
    let modifiers = modifiers(&spans);
    assert!(
        modifiers.contains(&Modifier::DIM),
        "远光带必须变暗：{modifiers:?}"
    );
    assert!(
        modifiers.contains(&Modifier::empty()),
        "中等强度保持普通：{modifiers:?}"
    );
    assert!(
        modifiers.contains(&Modifier::BOLD),
        "近光带必须加粗：{modifiers:?}"
    );
    assert!(
        spans.iter().all(|span| span.style.fg == Some(BASE)),
        "非 truecolor 全程使用基色，只换 modifier"
    );
}

#[test]
fn reduced_motion_is_one_static_span() {
    let spans = shimmer_spans(
        "Working",
        900,
        ShimmerStyle {
            animated: false,
            ..style(true)
        },
    );
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content.as_ref(), "Working");
    assert_eq!(spans[0].style.fg, Some(BASE));
    assert_eq!(spans[0].style.add_modifier, Modifier::empty());
    assert_eq!(
        spans,
        shimmer_spans(
            "Working",
            0,
            ShimmerStyle {
                animated: false,
                ..style(true)
            }
        ),
        "静态标签与相位无关"
    );
}

#[test]
fn cjk_text_keeps_one_span_per_character_and_its_display_width() {
    let spans = shimmer_spans("工作中", 400, style(true));
    assert_eq!(spans.len(), 3, "每字符一个 span，不按字节或显示列拆分");
    let width: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    assert_eq!(width, UnicodeWidthStr::width("工作中"));
}
