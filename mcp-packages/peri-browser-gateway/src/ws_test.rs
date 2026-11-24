use super::*;

#[test]
fn canvas_coordinates_map_to_css_pixels_and_clamp() {
    let meta = FrameMeta {
        viewport_width: 640,
        viewport_height: 480,
        device_width: 1280,
        device_height: 960,
        page_scale_factor: 2.0,
        scroll_offset_x: 0.0,
        scroll_offset_y: 0.0,
    };
    assert_eq!(
        canvas_to_css(100.0, 200.0, Some(&meta), None),
        (50.0, 100.0)
    );
    assert_eq!(
        canvas_to_css(
            1280.0,
            960.0,
            Some(&meta),
            Some(Viewport {
                width: 320,
                height: 240
            })
        ),
        (319.0, 239.0)
    );
    assert_eq!(
        canvas_to_css(5000.0, -20.0, Some(&meta), None),
        (639.0, 0.0)
    );
    assert_eq!(canvas_to_css(10.0, 20.0, None, None), (10.0, 20.0));
    assert_eq!(
        canvas_to_css(
            10.0,
            20.0,
            Some(&meta),
            Some(Viewport {
                width: 0,
                height: 0
            })
        ),
        (5.0, 10.0)
    );
}
