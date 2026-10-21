use super::test_support::*;
use super::*;
use peri_acp_types::history_window::HistoryCommittedTail;
use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;

fn commit_info(
    checkpoint: &str,
    tail_position: u64,
    generation: &str,
    structure: u64,
) -> HistoryCommitInfo {
    let tail = HistoryCommittedTail::new(
        &ThreadId::from("s1".to_string()),
        MessageId::new(),
        tail_position,
    )
    .encode();
    HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: generation.into(),
        structure_version: structure,
        checkpoint_id: checkpoint.into(),
        summary_entry_id: Some("s1:summary".into()),
        committed_tail: tail,
    }
}

#[test]
fn live_seal_without_position_stays_unsealed_until_page_promotes_it() {
    let mut window = enabled_window(WindowLimits::default());
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    key.clone(),
                    None,
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            )
            .durable(false)
            .origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live seal");
    assert_eq!(
        window.apply_commit(&commit_info("cp-before-position", 10, "gen-1", 7)),
        CommitOutcome::Applied { sealed_segments: 0 }
    );
    assert!(!window.segment(segment_key).unwrap().sealed);
    assert!(!window.segment(segment_key).unwrap().durable);
    assert!(
        window
            .segment(segment_key)
            .unwrap()
            .pins
            .contains(PinReason::Unpersisted)
    );
    assert!(window.promote_live_seal_position(&key, 10));
    assert_eq!(
        window.segment(segment_key).unwrap().entries[0].position,
        Some(10)
    );
    assert_eq!(
        window.apply_commit(&commit_info("cp-live", 10, "gen-1", 7)),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    let segment = window.segment(segment_key).unwrap();
    assert!(segment.sealed);
    assert!(segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));
}

#[test]
fn commit_tail_promotes_matching_live_entry_without_page() {
    let mut window = enabled_window(WindowLimits::default());
    let message_id = MessageId::new();
    let key = PersistentEntryKey::own(format!("s1:{}", message_id.as_uuid()));
    let segment_key = window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    key.clone(),
                    None,
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            )
            .durable(false)
            .origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live seal");
    let tail =
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), message_id, 10).encode();
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "cp-tail-promotes-live".into(),
        summary_entry_id: None,
        committed_tail: tail,
    };
    assert_eq!(
        window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    let segment = window.segment(segment_key).unwrap();
    assert!(segment.sealed);
    assert!(segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));
}

#[test]
fn same_checkpoint_seals_after_late_position_promotion() {
    let mut window = enabled_window(WindowLimits::default());
    let message_id = MessageId::new();
    let key = PersistentEntryKey::own(format!("s1:{}", message_id.as_uuid()));
    let segment_key = window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    key.clone(),
                    None,
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            )
            .durable(false)
            .origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live seal");
    let tail_message_id = MessageId::new();
    let tail =
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), tail_message_id, 10).encode();
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "cp-late-position".into(),
        summary_entry_id: None,
        committed_tail: tail,
    };

    // 1. 提交通知先到：checkpoint 被记录，但实时条目还没有 position。
    assert_eq!(
        window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 0 }
    );
    assert!(!window.segment(segment_key).unwrap().sealed);

    // 2. 页响应随后补齐 position（真实路径由 handle_window_page_loaded 完成）。
    window.promote_live_seal_position(&key, 10);

    // 3. 同一 checkpoint 再次到达：不得因 Duplicate 跳过封印与解除 pin。
    assert_eq!(
        window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    let segment = window.segment(segment_key).unwrap();
    assert!(segment.sealed);
    assert!(segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));

    // 4. 没有新增 durable 段时仍保持幂等。
    assert_eq!(window.apply_commit(&info), CommitOutcome::Duplicate);
}

#[test]
fn live_seal_promotion_ignores_unknown_identity() {
    let mut window = enabled_window(WindowLimits::default());
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(key, None, 1, 10, 20, Vec::new())],
                10,
                20,
            )
            .durable(false)
            .origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live seal");
    assert!(!window.promote_live_seal_position(&PersistentEntryKey::own("s1:missing"), 10));
    assert_eq!(
        window.segment(segment_key).unwrap().entries[0].position,
        None
    );
}
#[test]
fn commit_marks_only_segments_at_or_below_committed_tail() {
    let mut window = enabled_window(WindowLimits::default());
    let sealed_page = window
        .register_segment(segment("page", 5, 10, 20), SegmentPlacement::Tail)
        .expect("page");
    let sealed_live = window
        .register_segment(
            segment("live", 10, 10, 20).durable(false),
            SegmentPlacement::Tail,
        )
        .expect("live");
    let pending_live = window
        .register_segment(
            segment("pending", 20, 10, 20).durable(false),
            SegmentPlacement::Tail,
        )
        .expect("pending");
    let unmarked = window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![entry("unmarked", None, 1, 10, 20)],
                10,
                20,
            )
            .durable(false),
            SegmentPlacement::Tail,
        )
        .expect("unmarked");

    let info = commit_info("cp-1", 10, "gen-1", 7);
    let outcome = window.apply_commit(&info);
    assert_eq!(outcome, CommitOutcome::Applied { sealed_segments: 2 });
    assert!(window.segment(sealed_page).unwrap().sealed);
    assert!(window.segment(sealed_live).unwrap().sealed);
    assert!(window.segment(sealed_live).unwrap().durable);
    assert!(
        !window
            .segment(sealed_live)
            .unwrap()
            .pins
            .contains(PinReason::Unpersisted)
    );
    assert!(!window.segment(pending_live).unwrap().sealed);
    assert!(!window.segment(pending_live).unwrap().durable);
    assert!(!window.segment(unmarked).unwrap().sealed);
    assert_eq!(window.sealed_segment_count(), 2);
    assert_eq!(
        window.durable_watermark().unwrap().checkpoint_id.as_deref(),
        Some("cp-1")
    );
    assert_eq!(
        window.durable_watermark().unwrap().committed_tail,
        Some(info.committed_tail.clone())
    );
    assert!(window.budget().pinned_body_bytes < 40);
}

#[test]
fn commit_is_idempotent_by_checkpoint() {
    let mut window = enabled_window(WindowLimits::default());
    window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("segment");
    let info = commit_info("cp-1", 10, "gen-1", 7);
    assert_eq!(
        window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    assert_eq!(window.apply_commit(&info), CommitOutcome::Duplicate);
    assert_eq!(window.sealed_segment_count(), 1);
}

#[test]
fn stale_generation_or_structure_is_dropped_without_marking_checkpoint() {
    let mut window = enabled_window(WindowLimits::default());
    window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("segment");
    let mut stale = commit_info("cp-stale", 10, "gen-old", 7);
    assert_eq!(window.apply_commit(&stale), CommitOutcome::StaleGeneration);
    assert!(!window.is_checkpoint_seen("cp-stale"));
    assert_eq!(window.sealed_segment_count(), 0);

    stale.session_generation = "gen-1".into();
    stale.structure_version = 8;
    assert_eq!(window.apply_commit(&stale), CommitOutcome::StaleGeneration);
    assert!(!window.is_checkpoint_seen("cp-stale"));
}

#[test]
fn invalid_tail_does_not_burn_the_checkpoint() {
    let mut window = enabled_window(WindowLimits::default());
    window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("segment");
    let mut info = commit_info("cp-1", 10, "gen-1", 7);
    info.committed_tail = "not-a-tail".into();
    assert_eq!(window.apply_commit(&info), CommitOutcome::InvalidTail);
    assert!(!window.is_checkpoint_seen("cp-1"));

    info.committed_tail = commit_info("cp-1", 10, "gen-1", 7).committed_tail;
    assert_eq!(
        window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
}

#[test]
fn commit_without_open_window_is_ignored() {
    let mut window = HistoryWindow::new(HistoryWindowConfig::enabled());
    let outcome = window.apply_commit(&commit_info("cp-1", 10, "gen-1", 7));
    assert_eq!(outcome, CommitOutcome::UnknownWindow);
    assert_eq!(window.sealed_segment_count(), 0);
}

#[test]
fn retention_plan_keeps_viewport_and_one_adjacent_segment() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(segment("a", 5, 10, 20), SegmentPlacement::Tail)
        .expect("a");
    let b = window
        .register_segment(segment("b", 10, 10, 20), SegmentPlacement::Tail)
        .expect("b");
    let _c = window
        .register_segment(segment("c", 15, 10, 20), SegmentPlacement::Tail)
        .expect("c");
    let _d = window
        .register_segment(segment("d", 30, 10, 20), SegmentPlacement::Tail)
        .expect("d");
    assert_eq!(
        window.apply_commit(&commit_info("cp-1", 15, "gen-1", 7)),
        CommitOutcome::Applied { sealed_segments: 3 }
    );
    let a_entry = window.segment(a).unwrap().entries[0].key.clone();
    window.set_anchor(Some(WindowAnchor::new(
        a_entry,
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    let plans = window.retention_plan_after_commit();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].entries[0].entry_id, "s1:c");
    assert!(plans[0].segment != a);
    assert!(plans[0].segment != b);
}

#[test]
fn open_after_lost_commit_notification_realigns_window() {
    let mut window = enabled_window(WindowLimits::default());
    window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("segment");
    window.set_degraded("commit notification lost");
    let next = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let generation = window.begin_window(next.clone());
    assert_eq!(window.identity(), Some(&next));
    assert_eq!(window.segment_count(), 0);
    assert!(generation > 1);
    assert!(!window.is_degraded(), "successful open restores Ready");
    assert_eq!(window.generation(), generation);
}

#[test]
fn commit_info_decodes_the_s4_notification_shape() {
    let info = commit_info("cp-1", 10, "gen-1", 7);
    let raw = serde_json::json!({
        "sessionId": info.session_id,
        "sessionGeneration": info.session_generation,
        "structureVersion": info.structure_version,
        "checkpointId": info.checkpoint_id,
        "summaryEntryId": info.summary_entry_id,
        "committedTail": info.committed_tail,
        "hint": { "kind": "open", "anchor": { "kind": "latest" } }
    });
    assert_eq!(HistoryCommitInfo::decode(&raw), Some(info));
    assert!(HistoryCommitInfo::decode(&serde_json::json!({ "sessionId": "s1" })).is_none());
    assert!(matches!(
        crate::kit::acp_types::AcpEventData::decode("history-committed", raw),
        crate::kit::acp_types::AcpEventData::HistoryCommitted(_)
    ));
}
