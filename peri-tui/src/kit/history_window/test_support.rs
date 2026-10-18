//! `history_window` 测试共用构造器。

use super::*;

pub(crate) fn identity() -> WindowIdentity {
    WindowIdentity::new("s1", "gen-1", 7, "snap-1")
}

pub(crate) fn enabled_window(limits: WindowLimits) -> HistoryWindow {
    let mut window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits,
    });
    window.begin_window(identity());
    window
}

pub(crate) fn entry(
    id: &str,
    position: Option<u64>,
    units: usize,
    body_bytes: usize,
    reserved_bytes: usize,
) -> HistoryEntryView {
    HistoryEntryView::new(
        PersistentEntryKey::own(format!("s1:{id}")),
        position,
        units,
        body_bytes,
        reserved_bytes,
        Vec::new(),
    )
}

pub(crate) fn segment(
    id: &str,
    position: u64,
    body_bytes: usize,
    reserved_bytes: usize,
) -> SegmentDraft {
    segment_with_units(id, position, 1, body_bytes, reserved_bytes)
}

pub(crate) fn segment_with_units(
    id: &str,
    position: u64,
    units: usize,
    body_bytes: usize,
    reserved_bytes: usize,
) -> SegmentDraft {
    let entry = entry(id, Some(position), units, body_bytes, reserved_bytes);
    SegmentDraft::new(HistorySource::Own, vec![entry], body_bytes, reserved_bytes)
}

pub(crate) fn accepted_key(decision: AdmissionDecision) -> SegmentKey {
    match decision {
        AdmissionDecision::Accepted { mut segments, .. } => {
            assert_eq!(segments.len(), 1);
            segments.remove(0)
        }
        other => panic!("expected accepted page, got {other:?}"),
    }
}

pub(crate) fn admit(
    window: &mut HistoryWindow,
    request_id: &str,
    segment: SegmentDraft,
    placement: PagePlacement,
) -> AdmissionDecision {
    let reserved = segment.body_bytes;
    match window.reserve_page(request_id, reserved) {
        ReserveOutcome::Granted(_) => {}
        other => panic!("page reservation failed: {other:?}"),
    }
    window.admit_page(PageDraft {
        request_id: request_id.to_string(),
        window_generation: window.generation(),
        identity: identity(),
        placement,
        segments: vec![segment],
    })
}

pub(crate) fn admit_and_settle(
    window: &mut HistoryWindow,
    request_id: &str,
    segment: SegmentDraft,
) -> AdmissionDecision {
    let decision = admit(window, request_id, segment, PagePlacement::Tail);
    window.settle_pending_merge();
    decision
}

pub(crate) fn release_plan_for(window: &HistoryWindow, key: SegmentKey) -> ReleasePlan {
    let segment = window.segment(key).expect("segment");
    let (before, after) = window.neighbor_entries(key);
    ReleasePlan {
        segment: key,
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: window.index().slot_range(key),
        before,
        after,
        before_cursor: segment.before_cursor.clone(),
        after_cursor: segment.after_cursor.clone(),
    }
}
