//! 横向偏移状态：按围栏身份保存、按当前视口 clamp、帧整体可见时归零。

use super::{offset_for, reset_for_test, revision, scroll_by, set_offset};
use crate::kit::markdown::{MermaidFenceKey, MermaidMessageIdentity, MermaidMessageRef};

fn key(name: &str) -> MermaidFenceKey {
    MermaidFenceKey {
        message: MermaidMessageIdentity {
            session_id: "session".to_string(),
            message: MermaidMessageRef::Persistent(name.to_string()),
        },
        open_start: 12,
    }
}

#[test]
#[serial_test::serial]
fn offset_clamps_to_the_current_viewport_and_survives_streaming_growth() {
    reset_for_test();
    let key = key("msg");
    assert!(set_offset(&key, 30, 40, 100));
    assert_eq!(offset_for(&key, 40, 100), 30);
    // 流式变宽：保留阅读列。
    assert_eq!(offset_for(&key, 40, 140), 30);
    // 窗口变宽：clamp 到新的最大值。
    assert_eq!(offset_for(&key, 120, 140), 20);
    // 视口覆盖整图：回到 0。
    assert_eq!(offset_for(&key, 140, 140), 0);
}

#[test]
#[serial_test::serial]
fn narrowing_content_keeps_the_reading_column_while_it_still_overflows() {
    reset_for_test();
    let key = key("narrow");
    set_offset(&key, 40, 20, 100);
    assert_eq!(offset_for(&key, 20, 100), 40);
    // 流式变窄但仍溢出：保留阅读列，只按新上界 clamp。
    assert_eq!(offset_for(&key, 20, 60), 40);
    assert_eq!(offset_for(&key, 20, 50), 30, "clamp 到变窄后的最大值");
    // 帧整体可见才回到最左列，之后变宽从 0 开始。
    assert_eq!(offset_for(&key, 20, 20), 0);
    assert_eq!(offset_for(&key, 20, 100), 0);
}

#[test]
#[serial_test::serial]
fn resize_keeps_or_clamps_the_offset_instead_of_resetting() {
    reset_for_test();
    let key = key("resize");
    set_offset(&key, 40, 20, 100);
    // 窗口变宽 + 重排后帧宽变小：仍是同一张图，保留阅读列并 clamp。
    assert_eq!(offset_for(&key, 40, 80), 40);
    assert_eq!(offset_for(&key, 100, 80), 0, "视口覆盖整图时回到 0");
}

#[test]
#[serial_test::serial]
fn wheel_steps_clamp_at_both_ends_and_bump_the_revision() {
    reset_for_test();
    let key = key("wheel");
    let before = revision();
    assert!(scroll_by(&key, 8, 40, 100));
    assert_eq!(offset_for(&key, 40, 100), 8);
    assert!(revision() > before);
    for _ in 0..20 {
        scroll_by(&key, 8, 40, 100);
    }
    assert_eq!(offset_for(&key, 40, 100), 60);
    for _ in 0..20 {
        scroll_by(&key, -8, 40, 100);
    }
    assert_eq!(offset_for(&key, 40, 100), 0);
}

#[test]
#[serial_test::serial]
fn fitting_diagrams_do_not_allocate_state() {
    reset_for_test();
    let key = key("fits");
    assert_eq!(offset_for(&key, 60, 60), 0);
    assert!(
        !set_offset(&key, 0, 60, 60),
        "no visible movement, no revision"
    );
}
