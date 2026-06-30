// ─── P5: Pipeline-removed tests ──────────────────────────────────────────

/// Tests 1-2 (build_rebuild_all) removed — MessagePipeline::build_rebuild_all deleted.
/// Tests 4-8 (anchor insertion, discard, clamping) removed in Phase 2.5 —
/// ephemeral_notes anchor tracking retired. v2 state.view handles SystemNote
/// via pending_v2_notes → Event::PushSystemNote path.

/// Cron #26 step 7e.7: submit_message 不再写 v1 view_messages。
///
/// 历史行为：submit_message 调 apply_add_message(user_vm) 把 UserBubble push
/// 到 view_messages，并更新 round_start_vm_idx。但生产渲染只读 v2 state.view，
/// 所以这条 v1 写入是双写（用户感知不到，但是 Phase 2.6 收敛的债务）。
///
/// 新行为：v2 state.view 是单一数据源。UserBubble 由 SM 通过两条路径 push：
///   • Plain Enter（非 slash）：SM idle.rs Enter handler 直接 push（step 7d）
///   • Slash command Submit：keyboard 调 push_user_bubble 入队，main_loop
///     drain 后通过 Event::PushUserBubble 路由到 SM（cron #26 step 7e.7）
///
/// 此测试验证 submit_message 调用后 view_messages 不被写入。
#[tokio::test]
async fn test_submit_message_does_not_write_view_messages() {
    let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
    let initial_len = app.session_mgr.current().messages.view_messages.len();

    // 提交一条消息。submit_message 应该走 ACP 异步路径，但同步副作用
    // （historical: apply_add_message + round_start_vm_idx）必须已被删除。
    // 注意：headless 环境无 Provider，submit_message 会在 provider 检查处
    // early return 并 set_loading(false)。但 view_messages 写入发生在 provider
    // 检查之前（旧代码），所以本测试仍能有效验证 v1 push 已被删除。
    app.submit_message("hello world".to_string());

    let after_len = app.session_mgr.current().messages.view_messages.len();
    assert_eq!(
        after_len, initial_len,
        "submit_message must not push to view_messages (v2 state.view is single source). \
         before={}, after={}",
        initial_len, after_len
    );

    // last_human_message 在 provider 检查之前被设置，所以即使 early return
    // 也能验证。
    assert_eq!(
        app.session_mgr.current().metadata.last_human_message,
        Some("hello world".to_string()),
        "last_human_message should still be updated by submit_message"
    );
}

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
