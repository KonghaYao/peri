//! Bridge reset 必须同时清空 S5 历史窗口模型（会话边界清理）。

use super::*;
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
    SegmentDraft, SegmentPlacement, WindowIdentity,
};

#[test]
#[serial]
fn test_bridge_reset_clears_history_window() {
    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut state = scheduler_state();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 1, "snap-1"));
    state
        .window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    PersistentEntryKey::own("s1:m1"),
                    Some(1),
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            ),
            SegmentPlacement::Tail,
        )
        .expect("segment");
    assert_eq!(state.window.segment_count(), 1);

    let mut last_reset = old_reset;
    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    assert_eq!(state.window.segment_count(), 0);
    assert!(state.window.identity().is_none());
    assert_eq!(state.window.budget().resident_body_bytes, 0);
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
}
