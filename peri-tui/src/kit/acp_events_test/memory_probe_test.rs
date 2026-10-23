//! S1：TUI bridge 对象计量只读诊断。
//!
//! 只验证 committed / current_turn / folded_history 的条目与字节可区分；
//! 不改变任何释放/折叠语义。

use super::*;

use std::sync::Arc;

#[test]
fn bridge_snapshot_separates_committed_current_and_folded_bytes() {
    let mut state = make_fold_test_state();

    let committed_text = "committed-body-".repeat(16);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            committed_text.clone(),
        )));
    let assistant_text = "assistant-body-".repeat(32);
    state
        .committed
        .push_back(TuiRenderUnit::TuiAssistantBubble(Arc::new(
            TuiAssistantBubble {
                text: assistant_text.clone(),
                reasoning: None,
                message_id: None,
                started_at: None,
                duration_ms: None,
                content_hash: 0,
            },
        )));
    let current_text = "current-turn-body-".repeat(24);
    state.current_turn.append_text(&current_text, None);

    let snapshot = state.memory_snapshot();
    println!("Bridge T2=[{snapshot:?}]");

    assert_eq!(snapshot.committed_entries, 2);
    assert_eq!(snapshot.current_turn_entries, 1);
    assert_eq!(snapshot.folded_entries, 2);
    assert!(
        snapshot.committed_body_bytes >= committed_text.len() + assistant_text.len(),
        "committed 层必须计入两条正文"
    );
    assert!(
        snapshot.current_turn_body_bytes >= current_text.len(),
        "current_turn 层必须计入流式正文"
    );
    assert!(
        snapshot.folded_body_bytes >= committed_text.len() + assistant_text.len(),
        "folded 层必须与 committed 一样覆盖投影正文"
    );
    assert!(
        snapshot.unique_body_bytes
            <= snapshot.committed_body_bytes
                + snapshot.current_turn_body_bytes
                + snapshot.folded_body_bytes,
        "unique 是跨层去重后的下界，不能大于三层求和"
    );

    // 追加 current 正文不改变 committed / folded 的条目与字节。
    let committed_before = snapshot.committed_body_bytes;
    let folded_before = snapshot.folded_body_bytes;
    state.current_turn.append_text(&"next-".repeat(8), None);
    let after = state.memory_snapshot();
    assert_eq!(after.committed_entries, snapshot.committed_entries);
    assert_eq!(after.committed_body_bytes, committed_before);
    assert_eq!(after.folded_entries, snapshot.folded_entries);
    assert_eq!(after.folded_body_bytes, folded_before);
    assert!(
        after.current_turn_body_bytes > snapshot.current_turn_body_bytes,
        "current_turn 层必须反映新增流式正文"
    );
}

/// [回归测试] 同一 `Arc` 出现在多个展示层时，`unique_*` 只能计一次；
/// 槽位地址不能充当分配身份（每次 clone 的枚举槽位不同）。
#[test]
fn bridge_snapshot_dedups_shared_assistant_arc_across_layers() {
    let mut state = make_fold_test_state();
    let text = "shared-arc-body-".repeat(64);
    state
        .committed
        .push_back(TuiRenderUnit::TuiAssistantBubble(Arc::new(
            TuiAssistantBubble {
                text: text.clone(),
                reasoning: None,
                message_id: None,
                started_at: None,
                duration_ms: None,
                content_hash: 0,
            },
        )));

    let snapshot = state.memory_snapshot();
    assert_eq!(snapshot.committed_body_bytes, text.len());
    assert_eq!(snapshot.folded_body_bytes, text.len());
    assert_eq!(
        snapshot.unique_body_bytes,
        text.len(),
        "committed 与 folded 共享的 Arc 正文只能计入一次"
    );
    assert!(
        snapshot.deduped_allocations >= 1,
        "跨层共享分配必须被识别为去重命中"
    );
}

/// [M07 回归] 同一 Arc 同时出现在 current_turn 与 committed 时，unique 只计一次。
#[test]
fn bridge_snapshot_dedups_shared_arc_between_current_and_committed() {
    let mut state = make_fold_test_state();
    // PromptRunning 下 fold 不改写 bubble，folded_history 与 committed 共享同一 Arc。
    state.phase = SessionPhase::PromptRunning;
    let text = "shared-arc-cross-layer-".repeat(32);
    state.current_turn.append_text(&text, None);
    let shared = state
        .current_turn
        .view_models()
        .front()
        .expect("current bubble")
        .clone();
    state.committed.push_back(shared);

    let snapshot = state.memory_snapshot();
    assert_eq!(snapshot.committed_body_bytes, text.len());
    assert_eq!(snapshot.current_turn_body_bytes, text.len());
    assert_eq!(
        snapshot.unique_body_bytes,
        text.len(),
        "committed/current 共享同一 Arc 只能计入一次"
    );
    assert!(snapshot.deduped_allocations >= 1);
}

/// [M07 回归] 按值 clone 的 TuiToolCard 是独立分配，必须计两次。
#[test]
fn bridge_snapshot_counts_independently_cloned_tool_cards_twice() {
    let mut state = make_fold_test_state();
    let input = "input-summary-".repeat(64);
    let output = "output-summary-".repeat(64);
    let card = crate::kit::tui_render_unit::TuiToolCard {
        tool_id: "tool-clone".into(),
        tool_name: "Bash".into(),
        input_summary: input.clone(),
        output_summary: output.clone(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: crate::kit::tui_render_unit::TuiToolPresentation::Generic,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
        tool_calls_count: 0,
    };
    state
        .committed
        .push_back(TuiRenderUnit::TuiToolCard(card.clone()));
    state.committed.push_back(TuiRenderUnit::TuiToolCard(card));

    let snapshot = state.memory_snapshot();
    assert!(
        snapshot.committed_body_bytes >= 2 * (input.len() + output.len()),
        "独立 clone 的两张卡片必须分别计入"
    );
}

/// [M07 回归] 大 raw_input 进入原始累积器计量；mark_committed 清空后
/// body 归零但 String capacity 仍可区分。
#[test]
fn bridge_snapshot_separates_raw_input_and_capacity_after_clear() {
    let mut state = make_fold_test_state();
    let raw = "x".repeat(64 * 1024);
    state
        .current_turn
        .start_tool(crate::kit::acp_types::ToolCardAccumulator::with_input(
            "raw-tool".into(),
            "Bash".into(),
            "large".into(),
            serde_json::json!({"command": raw}),
            None,
        ));
    let with_raw = state.memory_snapshot();
    assert!(
        with_raw.current_turn_raw_body_bytes >= 64 * 1024,
        "大 raw_input 必须进入原始累积器计量"
    );
    assert!(
        with_raw.current_turn_raw_body_bytes > with_raw.current_turn_body_bytes,
        "投影层不含 raw_input，原始计量必须可区分"
    );

    let text = "capacity-body-".repeat(256);
    state.current_turn.append_text(&text, None);
    state.current_turn.mark_committed();
    let cleared = state.memory_snapshot();
    assert_eq!(
        cleared.current_turn_raw_body_bytes, 0,
        "清空后原始正文必须归零"
    );
    assert!(
        cleared.current_turn_raw_reserved_bytes >= text.len(),
        "clear 保留 String capacity，reserved 必须仍可观察"
    );
}

/// 本用例断言 BG live 的精确计数；保存并恢复全局表，
/// 避免前序用例残留条目影响断言，也避免清理动作泄漏给后续用例。
struct RestoreBgAtoms {
    identity: std::collections::HashMap<String, crate::kit::atoms::BgTaskIdentity>,
    live: std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
}

impl RestoreBgAtoms {
    fn clear() -> Self {
        let identity = crate::kit::atoms::BG_TASK_IDENTITY.state().read().clone();
        let live = crate::kit::atoms::BG_LIVE_DETAIL.state().read().clone();
        crate::kit::atoms::BG_TASK_IDENTITY.set(Default::default());
        crate::kit::atoms::BG_LIVE_DETAIL.set(Default::default());
        Self { identity, live }
    }
}

impl Drop for RestoreBgAtoms {
    fn drop(&mut self) {
        crate::kit::atoms::BG_TASK_IDENTITY.set(std::mem::take(&mut self.identity));
        crate::kit::atoms::BG_LIVE_DETAIL.set(std::mem::take(&mut self.live));
    }
}

/// [M07 回归] BG live detail 的 stream/nested 分层进入快照且不重复相加。
#[test]
#[serial]
fn bridge_snapshot_reports_bg_live_stream_and_nested_separately() {
    let _restore = RestoreBgAtoms::clear();
    crate::kit::atoms::BG_TASK_IDENTITY.state().write().insert(
        "probe-bg".into(),
        crate::kit::atoms::BgTaskIdentity {
            agent_id: Some("probe-agent".into()),
            ..Default::default()
        },
    );
    crate::kit::bg_task_live::init_agent_live_detail(
        "probe-bg",
        "probe-agent",
        "coder",
        Some("probe-instance"),
    );
    crate::kit::bg_task_live::append_bg_text_chunk(
        "probe-agent",
        &TuiTextChunk {
            text: "bg-stream-body-".repeat(64),
            message_id: Some("m".into()),
            agent_id: Some("probe-agent".into()),
        },
    );
    let mut state = make_fold_test_state();
    let before = state.memory_snapshot();
    assert_eq!(before.bg_live_entries, 1);
    assert!(before.bg_live_stream_bytes > 0);
    assert_eq!(before.bg_live_nested_units_bytes, 0);

    crate::kit::bg_task_live::publish_pending_streams();
    let after = state.memory_snapshot();
    assert!(
        after.bg_live_stream_bytes > 0,
        "投影后 stream 仍持有自己的 clone，必须计入"
    );
    assert!(after.bg_live_nested_units_bytes > 0);
    assert!(
        after.bg_live_body_bytes >= after.bg_live_nested_units_bytes + after.bg_live_stream_bytes
    );

    crate::kit::atoms::BG_TASK_IDENTITY
        .state()
        .write()
        .remove("probe-bg");
    crate::kit::atoms::BG_LIVE_DETAIL
        .state()
        .write()
        .remove("probe-bg");
}
