use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
use crate::kit::acp_types::ToolCardAccumulator;
use crate::kit::tui_render_unit::{FoldState, TuiToolCard};

fn card(turn: &mut CurrentTurn) -> &TuiToolCard {
    match &turn.view_models()[0] {
        TuiRenderUnit::TuiToolCard(card) => card,
        other => panic!("expected tool card, got {other:?}"),
    }
}

fn running_turn() -> CurrentTurn {
    let mut turn = CurrentTurn::new();
    turn.start_tool(ToolCardAccumulator::new(
        "tool".into(),
        "Bash".into(),
        "中文 input".repeat(512),
    ));
    turn
}

/// [回归测试] 相同秒内的 publication 不重建未变工具；跨秒刷新保留 fold 与 hash 契约。
#[tokio::test(start_paused = true)]
async fn test_tool_projection_reuses_unchanged_and_ticks_with_fold() {
    let mut turn = running_turn();
    let initial_hash = card(&mut turn).content_hash;
    reset_perf_counters();
    for _ in 0..40 {
        turn.invalidate_cache();
        assert_eq!(card(&mut turn).content_hash, initial_hash);
    }
    assert_eq!(perf_counters().tool_hash_calls, 0);
    if let TuiRenderUnit::TuiToolCard(card) = &mut turn.cached_view_models[0] {
        card.fold = FoldState::Expanded;
        card.user_modified = true;
        card.recompute_hash();
    }
    reset_perf_counters();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    turn.invalidate_cache();
    let updated = card(&mut turn).clone();
    assert_eq!(updated.running_duration_ms, Some(2000));
    assert_eq!(updated.fold, FoldState::Expanded);
    assert!(updated.user_modified);
    assert_ne!(updated.content_hash, initial_hash);
    assert_eq!(perf_counters().tool_hash_calls, 1);
    let mut recomputed = updated.clone();
    recomputed.recompute_hash();
    assert_eq!(updated.content_hash, recomputed.content_hash);
}

/// [回归测试] 工具输入升级与结束仍失效，已结束长输出不再反复构建或哈希。
#[tokio::test(start_paused = true)]
async fn test_tool_projection_input_upgrade_and_terminal_output() {
    let mut turn = running_turn();
    let initial_hash = card(&mut turn).content_hash;
    turn.start_tool(ToolCardAccumulator::with_input(
        "tool".into(),
        "Bash".into(),
        "updated".into(),
        serde_json::json!({"command": "true"}),
        None,
    ));
    assert_eq!(card(&mut turn).input_summary, "updated");
    assert_ne!(card(&mut turn).content_hash, initial_hash);
    tokio::time::advance(std::time::Duration::from_millis(1234)).await;
    assert!(turn.end_tool("tool", "失败输出".repeat(512), true));
    let completed = card(&mut turn).clone();
    assert!(!completed.is_running);
    assert!(completed.is_error);
    assert_eq!(completed.completed_duration_ms, Some(1234));
    assert_eq!(completed.fold, FoldState::Collapsed);
    reset_perf_counters();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    for _ in 0..40 {
        turn.invalidate_cache();
        assert_eq!(card(&mut turn).content_hash, completed.content_hash);
    }
    assert_eq!(perf_counters().tool_hash_calls, 0);
}

/// [回归测试] 无 ToolEnded 的取消必须退出 running，之后空输出不能反复失效。
#[tokio::test(start_paused = true)]
async fn test_tool_projection_cancel_without_output() {
    let mut turn = running_turn();
    assert!(card(&mut turn).is_running);
    turn.deactivate();
    let cancelled = card(&mut turn).clone();
    assert!(!cancelled.is_running);
    assert_eq!(cancelled.running_duration_ms, None);
    reset_perf_counters();
    turn.invalidate_cache();
    assert_eq!(card(&mut turn).content_hash, cancelled.content_hash);
    assert_eq!(perf_counters().tool_hash_calls, 0);
}
