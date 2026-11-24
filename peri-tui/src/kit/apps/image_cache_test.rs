use super::*;
use crate::kit::terminal_caps::{GraphicsProtocol, TerminalCaps};

fn key(seq: u64) -> FrameKey {
    FrameKey {
        view_id: "v1".to_string(),
        seq,
        cols: 40,
        rows: 20,
    }
}

#[test]
fn no_graphics_protocol_never_encodes() {
    let caps = TerminalCaps::default();
    assert_eq!(caps.graphics, GraphicsProtocol::None);
    let before = revision();
    request(
        key(1),
        std::sync::Arc::new(image::DynamicImage::new_rgb8(4, 4)),
        &caps,
        Some((8, 16)),
    );
    assert!(
        lookup_ready_for_canvas(&key(1)).is_none(),
        "无协议时不得产出图像"
    );
    assert!(!is_failed(&key(1)));
    assert_eq!(revision(), before, "无协议时不触发编码完成");
}

#[test]
fn recent_frame_reuse_ignores_sequence_but_not_view_or_canvas() {
    let current = key(9);
    assert!(same_canvas(&key(1), &current), "旧帧应在同一画布内复用");
    assert!(same_canvas(&current, &current));
    assert!(!same_canvas(
        &FrameKey {
            view_id: "v2".to_string(),
            ..key(1)
        },
        &current
    ));
    assert!(!same_canvas(&FrameKey { cols: 41, ..key(1) }, &current));
    assert!(!same_canvas(&FrameKey { rows: 21, ..key(1) }, &current));
}

#[test]
fn key_identity_covers_view_frame_and_area() {
    assert_eq!(key(1), key(1));
    assert_ne!(key(1), key(2));
    assert_ne!(
        key(1),
        FrameKey {
            view_id: "v2".to_string(),
            ..key(1)
        }
    );
    assert_ne!(key(1), FrameKey { cols: 41, ..key(1) });
}
