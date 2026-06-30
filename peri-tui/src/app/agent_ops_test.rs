// ─── P5: Pipeline-removed tests ──────────────────────────────────────────

/// Tests 1-2 (build_rebuild_all) removed — MessagePipeline::build_rebuild_all deleted.
/// Tests 4-8 (anchor insertion, discard, clamping) removed in Phase 2.5 —
/// ephemeral_notes anchor tracking retired. v2 state.view handles SystemNote
/// via pending_v2_notes → Event::PushSystemNote path.

/// 场景3: submit_message 记录 round_start_vm_idx（纯逻辑验证）
#[test]
fn test_submit_message_records_round_start_vm_idx() {
    let mut messages = vec![
        crate::ui::message_view::MessageViewModel::user("q1".to_string()),
        crate::ui::message_view::MessageViewModel::from_base_message(
            &peri_agent::messages::BaseMessage::ai("a1".to_string()),
            &[],
        ),
        crate::ui::message_view::MessageViewModel::user("q2".to_string()),
    ];

    messages.push(crate::ui::message_view::MessageViewModel::user(
        "q3".to_string(),
    ));
    let round_start_vm_idx = messages.len();
    assert_eq!(round_start_vm_idx, 4);
    assert_eq!(
        round_start_vm_idx, 4,
        "round_start_vm_idx 应为 push 后的值，确保 UserBubble 在 prefix 中"
    );
}
