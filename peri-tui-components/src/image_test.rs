//! Tests for the pure image component: size validation, aspect fitting and half blocks.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{ImageData, attachment_label, fit, halfblock_lines};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn rgba_image(width: u32, height: u32, pixels: &[[u8; 4]]) -> ImageData {
    assert_eq!(pixels.len(), (width * height) as usize);
    let rgba = pixels.iter().flatten().copied().collect();
    ImageData::new(width, height, rgba).expect("fixture dimensions must match its RGBA buffer")
}

fn base_style() -> Style {
    Style::new().fg(Color::White).bg(Color::Black)
}

fn only_span<'a>(line: &'a Line<'a>) -> &'a Span<'a> {
    assert_eq!(line.spans.len(), 1, "expected a single-cell line");
    &line.spans[0]
}

// ── ImageData ─────────────────────────────────────────────────────────────

#[test]
fn test_new_validates_rgba_length_and_dimensions() {
    let image = rgba_image(2, 2, &[RED, GREEN, BLUE, CLEAR]);
    assert_eq!((image.width(), image.height()), (2, 2));
    assert_eq!(image.rgba().len(), 16);

    assert!(
        ImageData::new(2, 2, vec![0; 15]).is_none(),
        "buffer shorter than width * height * 4 must be rejected"
    );
    assert!(
        ImageData::new(2, 2, vec![0; 17]).is_none(),
        "buffer longer than width * height * 4 must be rejected"
    );
    assert!(ImageData::new(0, 2, Vec::new()).is_none());
    assert!(ImageData::new(2, 0, Vec::new()).is_none());
    assert!(
        ImageData::new(u32::MAX, u32::MAX, Vec::new()).is_none(),
        "dimension overflow must not wrap into a valid length"
    );
}

// ── fit ───────────────────────────────────────────────────────────────────

#[test]
fn test_fit_width_limited_preserves_aspect() {
    // 300:200 image in 20x10 cells (10x20 px each) → 200px wide, 200x133 px.
    let fitted = fit((300, 200), (10, 20), (20, 10)).expect("valid bounds fit");
    assert_eq!((fitted.columns, fitted.rows), (20, 7));
    assert_eq!(fitted.pixels, (200, 133));
}

#[test]
fn test_fit_height_limited_preserves_aspect() {
    // Square image in 40x2 cells (10x20 px each) → only 40px tall.
    let fitted = fit((100, 100), (10, 20), (40, 2)).expect("valid bounds fit");
    assert_eq!((fitted.columns, fitted.rows), (4, 2));
    assert_eq!(fitted.pixels, (40, 40));
}

#[test]
fn test_fit_halfblock_uses_two_pixels_per_row() {
    // Half blocks are 1x2 px cells: 100x50 fits 40 columns and 10 rows.
    let fitted = fit((100, 50), (1, 2), (40, 20)).expect("valid bounds fit");
    assert_eq!((fitted.columns, fitted.rows), (40, 10));
    assert_eq!(fitted.pixels, (40, 20));
}

#[test]
fn test_fit_1x1_and_one_cell_bounds() {
    let fitted = fit((1, 1), (10, 20), (3, 2)).expect("valid bounds fit");
    assert_eq!((fitted.columns, fitted.rows), (3, 2));
    assert_eq!(fitted.pixels, (30, 30));

    let one_cell = fit((1, 1), (10, 20), (1, 1)).expect("one cell is still renderable");
    assert_eq!((one_cell.columns, one_cell.rows), (1, 1));
    assert_eq!(one_cell.pixels, (10, 10));
}

#[test]
fn test_fit_rejects_zero_inputs() {
    for image in [(0, 1), (1, 0)] {
        assert!(fit(image, (1, 2), (10, 10)).is_none(), "image={image:?}");
    }
    for cell in [(0, 2), (1, 0)] {
        assert!(fit((10, 10), cell, (10, 10)).is_none(), "cell={cell:?}");
    }
    for bounds in [(0, 10), (10, 0)] {
        assert!(fit((10, 10), (1, 2), bounds).is_none(), "bounds={bounds:?}");
    }
}

#[test]
fn test_fit_never_overflows_bounds_or_pixel_budget() {
    for image in [(1, 1), (1, 1000), (1000, 1), (1920, 1080), (7, 3)] {
        for cell in [(8, 16), (1, 2)] {
            for bounds in [(1, 1), (3, 2), (80, 24)] {
                let fitted = fit(image, cell, bounds).expect("non-zero inputs always fit");
                assert!(
                    (1..=bounds.0).contains(&fitted.columns),
                    "image={image:?} cell={cell:?} bounds={bounds:?} → {fitted:?}"
                );
                assert!(
                    (1..=bounds.1).contains(&fitted.rows),
                    "image={image:?} cell={cell:?} bounds={bounds:?} → {fitted:?}"
                );
                assert!(fitted.pixels.0 > 0 && fitted.pixels.1 > 0);
                assert!(u64::from(fitted.pixels.0) <= u64::from(bounds.0) * u64::from(cell.0));
                assert!(u64::from(fitted.pixels.1) <= u64::from(bounds.1) * u64::from(cell.1));
            }
        }
    }
}

// ── halfblock_lines ───────────────────────────────────────────────────────

#[test]
fn test_halfblock_draws_upper_and_lower_colors() {
    let image = rgba_image(1, 2, &[RED, BLUE]);
    let lines = halfblock_lines(&image, (1, 1), base_style());
    assert_eq!(lines.len(), 1);

    let span = only_span(&lines[0]);
    assert_eq!(span.content.as_ref(), "▀");
    assert_eq!(span.style.fg, Some(Color::Rgb(255, 0, 0)));
    assert_eq!(span.style.bg, Some(Color::Rgb(0, 0, 255)));
}

#[test]
fn test_halfblock_uses_full_block_for_equal_halves() {
    let image = rgba_image(1, 2, &[GREEN, GREEN]);
    let lines = halfblock_lines(&image, (1, 1), base_style());
    let span = only_span(&lines[0]);
    assert_eq!(span.content.as_ref(), "█");
    assert_eq!(span.style.fg, Some(Color::Rgb(0, 255, 0)));
}

#[test]
fn test_halfblock_transparent_cells_keep_base_style() {
    let image = rgba_image(1, 1, &[CLEAR]);
    let lines = halfblock_lines(&image, (2, 2), base_style());
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert_eq!(line.width(), 2);
        for span in &line.spans {
            assert_eq!(
                span.content.as_ref(),
                " ",
                "transparent pixel must stay blank"
            );
            assert_eq!(span.style, base_style());
        }
    }
}

#[test]
fn test_halfblock_alpha_threshold_and_single_halves() {
    let semi_transparent = [10, 20, 30, 127];
    let opaque = [40, 50, 60, 128];

    // 上半透明、下半不透明 → 只画下半块。
    let image = rgba_image(1, 2, &[semi_transparent, opaque]);
    let line = halfblock_lines(&image, (1, 1), base_style()).remove(0);
    let span = only_span(&line);
    assert_eq!(span.content.as_ref(), "▄");
    assert_eq!(span.style.fg, Some(Color::Rgb(40, 50, 60)));

    // 上不透明、下半透明 → 只画上半块。
    let image = rgba_image(1, 2, &[opaque, semi_transparent]);
    let line = halfblock_lines(&image, (1, 1), base_style()).remove(0);
    let span = only_span(&line);
    assert_eq!(span.content.as_ref(), "▀");
    assert_eq!(span.style.fg, Some(Color::Rgb(40, 50, 60)));
}

#[test]
fn test_halfblock_odd_height_keeps_bottom_sample() {
    // 3 source rows sampled into 4 half-block rows: centers map 0,1,1,2.
    let image = rgba_image(1, 3, &[RED, GREEN, BLUE]);
    let lines = halfblock_lines(&image, (1, 2), base_style());
    assert_eq!(lines.len(), 2);

    let first = only_span(&lines[0]);
    assert_eq!(first.content.as_ref(), "▀");
    assert_eq!(first.style.fg, Some(Color::Rgb(255, 0, 0)));
    assert_eq!(first.style.bg, Some(Color::Rgb(0, 255, 0)));

    let last = only_span(&lines[1]);
    assert_eq!(last.content.as_ref(), "▀");
    assert_eq!(last.style.fg, Some(Color::Rgb(0, 255, 0)));
    assert_eq!(last.style.bg, Some(Color::Rgb(0, 0, 255)));
}

#[test]
fn test_halfblock_width_budget_is_exact_for_1x1() {
    let image = rgba_image(1, 1, &[GREEN]);
    let lines = halfblock_lines(&image, (5, 3), base_style());
    assert_eq!(lines.len(), 3);
    for line in &lines {
        assert_eq!(line.spans.len(), 5);
        assert_eq!(line.width(), 5, "half blocks must stay single-width");
        assert!(line.spans.iter().all(|span| span.content.as_ref() == "█"));
    }
}

#[test]
fn test_halfblock_zero_area_is_empty() {
    let image = rgba_image(1, 1, &[GREEN]);
    assert!(halfblock_lines(&image, (0, 4), base_style()).is_empty());
    assert!(halfblock_lines(&image, (4, 0), base_style()).is_empty());
}

#[test]
fn test_halfblock_downsample_keeps_width() {
    let pixels: Vec<[u8; 4]> = (0..9).map(|index| [index as u8 * 20, 0, 0, 255]).collect();
    let image = rgba_image(3, 3, &pixels);
    let lines = halfblock_lines(&image, (2, 1), base_style());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].width(), 2);
    assert_eq!(lines[0].spans.len(), 2);
}

// ── attachment label ──────────────────────────────────────────────────────

#[test]
fn test_attachment_label_is_one_based() {
    assert_eq!(attachment_label(1), "[Image #1]");
    assert_eq!(attachment_label(2), "[Image #2]");
    assert_eq!(attachment_label(12), "[Image #12]");
}
