use super::{
    agent_table_header, clamp_run_selection, index_for_run_id, model_cell, truncate_to_width,
};

#[test]
fn test_index_for_run_id_selects_matching_run() {
    use crate::kit::workflow_snapshot::TuiRunProgress;
    let runs = vec![
        TuiRunProgress {
            run_id: "a".into(),
            ..Default::default()
        },
        TuiRunProgress {
            run_id: "b".into(),
            ..Default::default()
        },
    ];
    assert_eq!(index_for_run_id(&runs, "b"), Some(1));
    assert_eq!(index_for_run_id(&runs, "missing"), None);
}

#[test]
fn test_index_for_run_id_missing_clamps_zero() {
    use crate::kit::workflow_snapshot::TuiRunProgress;
    let runs = vec![TuiRunProgress {
        run_id: "only".into(),
        ..Default::default()
    }];
    assert!(index_for_run_id(&runs, "gone").is_none());
    assert_eq!(clamp_run_selection(0, runs.len()), 0);
}

/// [回归测试] workflow 轮询快照收缩时，旧的选中 tab 不能越界。
///
/// 历史背景：后台 workflow 回调后的快照从多条 run 收缩为一条时，
/// `WorkflowPanel` 直接以旧 `active_run` 索引列表，导致 TUI panic。
#[test]
fn test_clamp_run_selection_after_snapshot_shrinks() {
    assert_eq!(clamp_run_selection(1, 1), 0);
    assert_eq!(clamp_run_selection(2, 3), 2);
    assert_eq!(clamp_run_selection(0, 0), 0);
}

#[test]
fn test_agent_table_header_model_column_aligns_with_row_prefix() {
    let header = agent_table_header("Model");
    let model_start = header.find("Model").expect("model header");
    assert_eq!(
        model_start,
        super::AGENT_ROW_LEADING_COLS + super::AGENT_NAME_COL_WIDTH
    );
}

/// [单测] Model 列 Unicode 宽度截断 helper：ASCII/CJK 均按终端显示宽度
/// 截断（CJK 每字符 2 列），超宽时以 '…'（1 列）收尾。
#[test]
fn test_truncate_to_width_unicode_safe() {
    assert_eq!(truncate_to_width("short", 12), "short");
    assert_eq!(truncate_to_width("claude-sonnet-4-5", 12), "claude-sonn…");
    assert_eq!(truncate_to_width("中文模型名", 6), "中文…");
    assert_eq!(truncate_to_width("ab", 1), "…");
}

/// [单测] Model 列单元格：缺失显示 '-'，未超宽补齐到列宽，超宽截断。
#[test]
fn test_model_cell_missing_padding_and_truncate() {
    assert_eq!(model_cell(None, 12), format!("-{}", " ".repeat(11)));
    assert_eq!(model_cell(Some("claude-haiku"), 12), "claude-haiku");
    assert_eq!(
        model_cell(Some("haiku"), 12),
        format!("haiku{}", " ".repeat(7))
    );
    assert_eq!(model_cell(Some("claude-sonnet-4-5"), 12), "claude-sonn…");
}
