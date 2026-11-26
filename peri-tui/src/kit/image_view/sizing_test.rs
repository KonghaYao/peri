//! 等比铺满几何：受限轴贴边、放大上限、floor 溢出保护、居中与极端比例。

use super::{Budget, Cells, Fill, fit};

fn cells(iw: u32, ih: u32, cell: (u16, u16), cols: u16, rows: u16, fill: Fill) -> Cells {
    fit(iw, ih, cell, Budget::new(cols, rows), fill).expect("合法参数应得到几何")
}

#[test]
fn tall_image_hits_row_budget_and_centers() {
    // 1600×900 在 80×8 区域内：高度受限（8 行 = 160px），宽度按比例 284px→29 格。
    let c = cells(1600, 900, (10, 20), 80, 8, Fill::NoUpscale);
    assert_eq!(c.rows, 8, "高度受限轴必须贴满行预算");
    assert!(
        (28..=29).contains(&c.cols),
        "宽度按等比换算，实际 {}",
        c.cols
    );
    assert_eq!(c.x_offset, (80 - c.cols) / 2, "水平居中");
}

#[test]
fn sideways_image_fills_width() {
    let c = cells(4000, 200, (10, 20), 80, 8, Fill::NoUpscale);
    assert_eq!(c.cols, 80, "宽度受限轴必须贴满列预算");
    assert!(c.rows <= 8);
}

#[test]
fn upscale_cap_fills_small_image_to_budget() {
    // 200×100（2:1）在 80×8 区域内：高度先贴边（th=160→8 行），宽度 320px→32 格。
    let c = cells(200, 100, (10, 20), 80, 8, Fill::Upscale2x);
    assert_eq!((c.cols, c.rows), (32, 8), "2 倍上限内应放大铺满");
    assert_eq!(c.x_offset, 24);
}

#[test]
fn no_upscale_keeps_small_image_natural_cells() {
    let c = cells(30, 10, (10, 20), 80, 8, Fill::NoUpscale);
    assert_eq!((c.cols, c.rows), (3, 1), "不放大时按自然格数显示");
    assert_eq!(c.x_offset, (80 - 3) / 2);
}

#[test]
fn upscale_never_exceeds_two_times_natural_cells() {
    // 4×4 像素：自然 1×1 格；2 倍上限 → 最多 2×2 格（像素 20×40）。
    let c = cells(4, 4, (10, 20), 80, 8, Fill::Upscale2x);
    assert!(c.cols <= 2 && c.rows <= 2, "放大不得超过 2x 自然格数");
}

#[test]
fn tiny_and_extreme_images_stay_inside_budget() {
    let one = cells(1, 1, (10, 20), 80, 8, Fill::Upscale2x);
    assert_eq!((one.cols, one.rows), (1, 1), "1×1 像素至少占 1 格");

    let strip = cells(1000, 10, (10, 20), 80, 8, Fill::Upscale2x);
    assert_eq!(strip.cols, 80);
    assert_eq!(strip.rows, 1, "100:1 横条按比例只占 1 行");

    let tower = cells(10, 1000, (10, 20), 80, 8, Fill::Upscale2x);
    assert_eq!(tower.rows, 8);
    assert_eq!(tower.cols, 1);
    assert_eq!(tower.x_offset, 39, "竖条在区域中央");
}

#[test]
fn limited_axis_reaches_budget_for_many_ratios() {
    let cell = (8, 17);
    let (cols, rows) = (60_u16, 6_u16);
    let avail_w = f32::from(cols) * f32::from(cell.0);
    let avail_h = f32::from(rows) * f32::from(cell.1);
    for (iw, ih) in [
        (1920, 1080),
        (800, 600),
        (300, 900),
        (64, 64),
        (1000, 500),
        (7, 5),
    ] {
        for fill in [Fill::NoUpscale, Fill::Upscale2x] {
            let c = cells(iw, ih, cell, cols, rows, fill);
            let budget_limited = (avail_w / iw as f32).min(avail_h / ih as f32) <= fill.max_scale();
            if budget_limited {
                // 缩小到预算内时，至少一个受限轴必须贴边（floor+ceil 的容差 ≤1 格）。
                assert!(
                    c.cols == cols || c.rows == rows,
                    "({iw}x{ih}) {fill:?} 未贴边: {c:?}"
                );
            }
            assert!(c.cols <= cols && c.rows <= rows, "几何不得越过预算");
            assert!(c.cols >= 1 && c.rows >= 1);
        }
    }
}

#[test]
fn invalid_inputs_return_none() {
    assert!(fit(0, 10, (10, 20), Budget::new(80, 8), Fill::Upscale2x).is_none());
    assert!(fit(10, 0, (10, 20), Budget::new(80, 8), Fill::Upscale2x).is_none());
    assert!(fit(10, 10, (0, 20), Budget::new(80, 8), Fill::Upscale2x).is_none());
    assert!(fit(10, 10, (10, 0), Budget::new(80, 8), Fill::Upscale2x).is_none());
    assert!(fit(10, 10, (10, 20), Budget::new(0, 8), Fill::Upscale2x).is_none());
    assert!(fit(10, 10, (10, 20), Budget::new(80, 0), Fill::Upscale2x).is_none());
}
