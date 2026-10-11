use super::*;
use ratatui::backend::TestBackend;
use std::task::{Context, Poll, Waker};

fn draw_tracker(tracker: &mut MsgAreaTracker, area: Rect) {
    let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 60)).unwrap();
    terminal
        .draw(|frame| {
            let mut drawer = ComponentDrawer::new(frame, area);
            tracker.pre_component_draw(&mut drawer);
        })
        .unwrap();
}

#[test]
fn initial_message_area_geometry_requests_one_corrective_frame() {
    let mut tracker = MsgAreaTracker::new();
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(tracker.poll_change(&mut context), Poll::Pending);

    let area = Rect::new(4, 2, 80, 24);
    draw_tracker(&mut tracker, area);
    assert_eq!(tracker.rect, Some(area));
    assert_eq!(tracker.poll_change(&mut context), Poll::Ready(()));
    assert_eq!(tracker.poll_change(&mut context), Poll::Pending);
}

#[test]
fn message_area_geometry_changes_request_frames_without_content_updates() {
    let mut tracker = MsgAreaTracker::new();
    let mut context = Context::from_waker(Waker::noop());
    draw_tracker(&mut tracker, Rect::new(4, 2, 80, 12));
    let _ = tracker.poll_change(&mut context);

    for area in [
        Rect::new(4, 2, 80, 24),
        Rect::new(4, 2, 80, 8),
        Rect::new(2, 2, 40, 8),
        Rect::new(2, 5, 40, 8),
    ] {
        draw_tracker(&mut tracker, area);
        assert_eq!(tracker.rect, Some(area));
        assert_eq!(tracker.poll_change(&mut context), Poll::Ready(()));
        draw_tracker(&mut tracker, area);
        assert_eq!(tracker.poll_change(&mut context), Poll::Pending);
    }
}
