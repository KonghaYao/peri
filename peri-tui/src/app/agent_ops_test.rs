// ─── P5: Pipeline-removed tests ──────────────────────────────────────────

/// Tests 1-2 (build_rebuild_all) removed — MessagePipeline::build_rebuild_all deleted.
/// Tests 4-8 (anchor insertion, discard, clamping) removed in Phase 2.5 —
/// ephemeral_notes anchor tracking retired. v2 state.view handles SystemNote
/// via pending_v2_notes → Event::PushSystemNote path.

/// Cron #46 (Phase 2.6 step 7e.9): `test_submit_message_does_not_write_view_messages`
/// removed — the field it asserted on (`view_messages`) is being deleted. The
/// regression it guarded (submit_message pushing to v1 view_messages) is now
/// structurally impossible because apply_add_message has been deleted and the
/// field itself will be gone.

/// Cron #26 step 7e.7: round_start_vm_idx 不再被 submit_message 更新。
///
/// 历史上 submit_message 把 round_start_vm_idx 设为 view_messages.len()
/// （在 push UserBubble 之后）。删除 v1 push 后，view_messages.len() 不再
/// 增长，所以 round_start_vm_idx 也应保持原值。handle_interrupted /
/// handle_done 都已迁到 v2 view_store 扫描（cron #23 P1 #1 + step 7c），
/// 不读 round_start_vm_idx，所以这个字段的陈旧性对生产无影响。
#[tokio::test]
async fn test_submit_message_does_not_bump_round_start_vm_idx() {
    let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
    // 预设一个非默认值，验证 submit_message 不会改写它。
    app.session_mgr.current_mut().messages.round_start_vm_idx = 7;

    app.submit_message("test".to_string());

    assert_eq!(
        app.session_mgr.current().messages.round_start_vm_idx,
        7,
        "submit_message must not bump round_start_vm_idx (v1 field, retired in step 7e.7)"
    );
}
