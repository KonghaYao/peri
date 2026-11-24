use super::*;
use crate::kit::apps::view_client::{FrameEvent, ViewSessionStatus};
use serde_json::json;

fn entry(app_id: &str, tool_call_id: Option<&str>) -> AppEntry {
    AppEntry {
        session_id: "session-1".to_string(),
        app_id: app_id.to_string(),
        server_id: "demo".to_string(),
        resource_uri: "ui://get-time/mcp-app.html".to_string(),
        instantiating_tool: "get-time".to_string(),
        tool_call_id: tool_call_id.map(str::to_string),
    }
}

fn status(state: &str, code: Option<&str>) -> ViewSessionStatus {
    ViewSessionStatus {
        state: state.to_string(),
        code: code.map(str::to_string),
        frames: 0,
        pending_requests: 0,
    }
}

fn frame(seq: u64) -> FrameImage {
    FrameImage {
        seq,
        width: 1280,
        height: 720,
        viewport_width: 1280,
        viewport_height: 720,
        image: std::sync::Arc::new(image::DynamicImage::new_rgb8(4, 4)),
    }
}

#[test]
fn lifecycle_starting_ready_frame_closed() {
    let mut view = AppView::new("v1", entry("app-1", Some("call-1")));
    assert_eq!(view.phase, ViewPhase::Starting);

    assert!(view.apply_status("ready", None));
    assert_eq!(view.phase, ViewPhase::Ready);

    assert!(view.apply_frame(frame(1)));
    assert_eq!(view.frame.as_ref().map(|f| f.seq), Some(1));

    assert!(view.apply_event(&ViewEvent::Closed {
        reason: Some("deleted".to_string()),
    }));
    assert_eq!(
        view.phase,
        ViewPhase::Closed {
            reason: Some("deleted".to_string())
        }
    );
    assert!(!view.apply_frame(frame(2)), "终态不再接受帧");
}

#[test]
fn failed_state_keeps_only_the_code() {
    let mut view = AppView::new("v1", entry("app-1", None));
    assert!(view.apply_event(&ViewEvent::Status(status(
        "failed",
        Some("navigation_failed")
    ))));
    assert_eq!(
        view.phase,
        ViewPhase::Failed {
            code: "navigation_failed".to_string()
        }
    );
    assert_eq!(view.phase.error_code(), Some("navigation_failed"));
    // 迟到的 ready 不回滚失败态。
    assert!(!view.apply_status("ready", None));
}

#[test]
fn out_of_order_and_duplicate_frames_are_ignored() {
    let mut view = AppView::new("v1", entry("app-1", None));
    assert!(view.apply_frame(frame(5)));
    assert!(!view.apply_frame(frame(4)));
    assert!(!view.apply_frame(frame(5)));
    assert_eq!(view.frame.as_ref().map(|f| f.seq), Some(5));
    assert!(view.apply_frame(frame(6)));
    assert_eq!(view.frame.as_ref().map(|f| f.seq), Some(6));
}

#[test]
fn starting_does_not_regress_ready_view() {
    let mut view = AppView::new("v1", entry("app-1", None));
    assert!(view.apply_status("ready", None));
    assert!(!view.apply_status("starting", None));
    assert_eq!(view.phase, ViewPhase::Ready);
}

#[test]
fn app_request_increments_pending_count_only() {
    let mut view = AppView::new("v1", entry("app-1", None));
    let request = super::super::request_queue::PendingAppRequest {
        seq: 1,
        id: json!(1),
        method: "tools/call".to_string(),
        params: json!({"name": "get-time"}),
    };
    assert!(view.apply_event(&ViewEvent::AppRequest(request)));
    assert_eq!(view.pending_requests, 1);
    assert_eq!(view.phase, ViewPhase::Starting);
}

#[test]
fn frame_event_alone_does_not_mutate_state() {
    // 帧解码是 runtime 的阻塞线程职责；`apply_event(Frame)` 必须无副作用。
    let mut view = AppView::new("v1", entry("app-1", None));
    assert!(!view.apply_event(&ViewEvent::Frame(FrameEvent {
        seq: 1,
        width: 1,
        height: 1,
        viewport_width: 1,
        viewport_height: 1,
        data_base64: String::new(),
    })));
    assert!(view.frame.is_none());
}

#[test]
fn multiple_views_keep_independent_attribution() {
    let mut first = AppView::new("v1", entry("app-1", Some("call-1")));
    let mut second = AppView::new("v2", entry("app-2", Some("call-2")));
    // 反向回执：后打开的视图先 ready/帧，先打开的视图后关闭。
    second.apply_status("ready", None);
    second.apply_frame(frame(10));
    first.apply_status("ready", None);
    first.apply_frame(frame(20));
    first.apply_event(&ViewEvent::Closed { reason: None });

    assert_eq!(second.frame.as_ref().map(|f| f.seq), Some(10));
    assert_eq!(second.phase, ViewPhase::Ready);
    assert!(matches!(first.phase, ViewPhase::Closed { .. }));
    assert_eq!(first.app.app_id, "app-1");
    assert_eq!(second.app.app_id, "app-2");
}

#[serial_test::serial]
#[test]
fn registry_lookup_requires_matching_session_and_tool_call() {
    reset_for_test();
    publish_descriptors(vec![entry("app-1", Some("call-1"))]);
    assert!(app_for_tool_call("session-1", "call-1").is_some());
    assert!(app_for_tool_call("session-2", "call-1").is_none());
    assert!(app_for_tool_call("session-1", "call-2").is_none());
    assert!(app_for_tool_call("", "call-1").is_none());
}

#[serial_test::serial]
#[test]
fn descriptor_revision_bumps_only_on_change() {
    reset_for_test();
    let before = registry_revision();
    publish_descriptors(vec![entry("app-1", None)]);
    let after_first = registry_revision();
    assert!(after_first > before);
    publish_descriptors(vec![entry("app-1", None)]);
    assert_eq!(registry_revision(), after_first, "重复发布不 bump");
    publish_descriptors(vec![]);
    assert!(registry_revision() > after_first);
}

#[serial_test::serial]
#[test]
fn insert_and_cycle_multiple_views() {
    reset_for_test();
    insert_view(AppView::new("v1", entry("app-1", None)));
    insert_view(AppView::new("v2", entry("app-2", None)));
    assert_eq!(
        APPS_STATE.state().read().active_view.as_deref(),
        Some("v2"),
        "最新打开的视图成为活动视图"
    );
    cycle_active_view(1);
    assert_eq!(APPS_STATE.state().read().active_view.as_deref(), Some("v1"));
    cycle_active_view(-1);
    assert_eq!(APPS_STATE.state().read().active_view.as_deref(), Some("v2"));
    cycle_active_view(-1);
    assert_eq!(APPS_STATE.state().read().active_view.as_deref(), Some("v1"));

    remove_view("v2");
    assert_eq!(
        APPS_STATE.state().read().active_view.as_deref(),
        Some("v1"),
        "关闭活动视图后顺延到剩余视图"
    );
}

#[serial_test::serial]
#[test]
fn live_view_for_app_ignores_closed_views() {
    reset_for_test();
    let mut closed = AppView::new("v1", entry("app-1", None));
    closed.phase = ViewPhase::Closed { reason: None };
    insert_view(closed);
    assert!(
        APPS_STATE
            .state()
            .read()
            .live_view_for_app("app-1")
            .is_none()
    );
    let mut live = AppView::new("v2", entry("app-1", None));
    live.phase = ViewPhase::Ready;
    insert_view(live);
    assert!(
        APPS_STATE
            .state()
            .read()
            .live_view_for_app("app-1")
            .is_some()
    );
}

#[serial_test::serial]
#[test]
fn concurrent_view_updates_do_not_lose_writes() {
    reset_for_test();
    insert_view(AppView::new("v1", entry("app-1", None)));
    insert_view(AppView::new("v2", entry("app-2", None)));

    let handles: Vec<_> = ["v1", "v2"]
        .into_iter()
        .map(|view_id| {
            std::thread::spawn(move || {
                for _ in 0..200 {
                    update_view(view_id, |view| {
                        view.next_seq += 1;
                        true
                    });
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("worker");
    }

    let state = APPS_STATE.state().read().clone();
    assert_eq!(state.view("v1").unwrap().next_seq, 200);
    assert_eq!(state.view("v2").unwrap().next_seq, 200);
}

#[test]
fn closed_event_does_not_mask_a_failure_code() {
    let mut view = AppView::new("v1", entry("app-1", None));
    view.apply_status("failed", Some("navigation_failed"));
    assert!(!view.apply_event(&ViewEvent::Closed {
        reason: Some("start_failed".to_string()),
    }));
    assert_eq!(
        view.phase,
        ViewPhase::Failed {
            code: "navigation_failed".to_string()
        }
    );
    assert_eq!(view.last_error.as_deref(), Some("navigation_failed"));
}
