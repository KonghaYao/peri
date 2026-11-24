use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
};

fn frame_geometry(width: u32, height: u32, vw: u32, vh: u32) -> FrameGeometry {
    FrameGeometry {
        width,
        height,
        viewport_width: vw,
        viewport_height: vh,
    }
}

#[test]
fn fits_wide_frame_to_full_width_and_centers_vertically() {
    // 16:9 帧进 40x20 单元格：满宽 320px → 高 180px → 11 行；垂直居中。
    let fit = fit_image(
        &frame_geometry(1280, 720, 1280, 720),
        Rect::new(0, 0, 40, 20),
        CELL,
    )
    .expect("fit");
    assert_eq!(fit.cols, 40);
    assert_eq!(fit.rows, 11);
    assert_eq!((fit.x, fit.y), (0, (20 - 11) / 2));
}

#[test]
fn fits_tall_frame_to_full_height_and_centers_horizontally() {
    // 1:2 帧进 40x20：满高 20 行 = 320px 高 → 宽 160px → 20 列。
    let fit = fit_image(
        &frame_geometry(400, 800, 400, 800),
        Rect::new(5, 3, 40, 20),
        CELL,
    )
    .expect("fit");
    assert_eq!(fit.cols, 20);
    assert_eq!(fit.rows, 20);
    assert_eq!(fit.x, 5 + (40 - 20) / 2);
    assert_eq!(fit.y, 3);
}

#[test]
fn maps_cell_centers_to_viewport_css() {
    let geometry = frame_geometry(1280, 720, 1280, 720);
    let fit = fit_image(&geometry, Rect::new(0, 0, 40, 20), CELL).expect("fit");
    let point = cell_to_viewport(&geometry, &fit, fit.x, fit.y).expect("inside");
    // 40 列覆盖 1280 CSS px：首列中心 = 0.5/40 * 1280 = 16。
    assert!((point.x - 16.0).abs() < 0.01, "{point:?}");
    assert!(point.y > 0.0 && point.y < 720.0 / 11.0, "{point:?}");
    let last = cell_to_viewport(&geometry, &fit, fit.x + fit.cols - 1, fit.y + fit.rows - 1)
        .expect("inside");
    // 末列中心 = 39.5/40 * 1280 = 1264；末行中心 = 10.5/11 * 720。
    assert!((last.x - 1264.0).abs() < 0.01, "{last:?}");
    assert!((last.y - 720.0 * 10.5 / 11.0).abs() < 0.01, "{last:?}");
}

#[test]
fn out_of_bounds_cells_return_none() {
    let geometry = frame_geometry(1280, 720, 1280, 720);
    let fit = fit_image(&geometry, Rect::new(10, 4, 40, 20), CELL).expect("fit");
    assert!(cell_to_viewport(&geometry, &fit, 9, 4).is_none());
    assert!(cell_to_viewport(&geometry, &fit, 10, 3).is_none());
    assert!(cell_to_viewport(&geometry, &fit, 50, 4).is_none());
    assert!(cell_to_viewport(&geometry, &fit, 10, 24).is_none());
}

#[test]
fn degenerate_inputs_return_none() {
    let geometry = frame_geometry(0, 720, 1280, 720);
    assert!(fit_image(&geometry, Rect::new(0, 0, 40, 20), CELL).is_none());
    assert!(
        fit_image(
            &frame_geometry(1280, 720, 1280, 720),
            Rect::new(0, 0, 0, 20),
            CELL
        )
        .is_none()
    );
    let zero_viewport = frame_geometry(1280, 720, 0, 720);
    let fit = fit_image(&zero_viewport, Rect::new(0, 0, 40, 20), CELL).expect("fit");
    assert!(cell_to_viewport(&zero_viewport, &fit, 0, 0).is_none());
}

#[test]
fn convenience_function_matches_two_step_mapping() {
    let geometry = frame_geometry(800, 600, 400, 300);
    let area = Rect::new(2, 1, 30, 12);
    let direct = terminal_cell_to_viewport(&geometry, area, CELL, 12, 6).expect("mapped");
    let fit = fit_image(&geometry, area, CELL).expect("fit");
    let two_step = cell_to_viewport(&geometry, &fit, 12, 6).expect("mapped");
    assert_eq!(direct, two_step);
    // 截图 800x600 与 CSS 400x300 比例一致：比例映射落在视口内。
    assert!(direct.x >= 0.0 && direct.x <= 400.0);
    assert!(direct.y >= 0.0 && direct.y <= 300.0);
}
