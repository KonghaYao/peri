//! 三段行表推导与取消动作解析（纯函数，不经挂载）。

use super::bg_task;
use crate::kit::panels::tasks::viewport::{
    CancelAction, CancelTarget, SegmentCounts, TaskSegment, TasksRow, TasksViewport,
};

/// 段标题与段间空行按段存在性出现：固定行不是常量，空态另有 3 行。
#[test]
fn row_table_follows_segment_presence() {
    let empty = TasksViewport::for_content_rows(16, SegmentCounts::default(), 0);
    assert_eq!(
        empty.rows(),
        &[
            TasksRow::Summary,
            TasksRow::Blank,
            TasksRow::EmptyTitle,
            TasksRow::EmptyHintCron,
            TasksRow::EmptyHintSubagent,
            TasksRow::Blank,
            TasksRow::NavHint,
        ],
        "空态：摘要 + 空行 + 3 行空态 + 空行 + 提示"
    );

    let cron_only = TasksViewport::for_content_rows(
        16,
        SegmentCounts {
            background: 0,
            cron: 2,
            subagent: 0,
        },
        0,
    );
    assert_eq!(
        cron_only.rows(),
        &[
            TasksRow::Summary,
            TasksRow::Blank,
            TasksRow::SectionHeader(TaskSegment::Cron),
            TasksRow::Item(0),
            TasksRow::Item(1),
            TasksRow::Blank,
            TasksRow::NavHint,
        ],
        "只有 Cron 段时不得出现空段标题，段后只有一处空行（段间空行不是段尾空行）"
    );
}

/// 可见窗口跟随选中项：窗口外的段不渲染（段标题与段间空行同存同缺）。
#[test]
fn window_follows_selection() {
    let counts = SegmentCounts {
        background: 8,
        cron: 3,
        subagent: 2,
    };
    // 内容区 16 行、三段固定行 9 行 → 可见 7 项
    let viewport = TasksViewport::for_content_rows(16, counts, 6);
    let items: Vec<usize> = viewport
        .rows()
        .iter()
        .filter_map(|row| match row {
            TasksRow::Item(index) => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        vec![4, 5, 6, 7, 8, 9, 10],
        "窗口须含选中项且不超出容量"
    );
    assert!(
        !viewport
            .rows()
            .contains(&TasksRow::SectionHeader(TaskSegment::SubAgent)),
        "窗口外的段标题不得渲染"
    );
}

/// 面板矮于固定行时按预算截行：行表不得给出屏幕外的命中。
#[test]
fn row_table_respects_budget() {
    let counts = SegmentCounts {
        background: 8,
        cron: 0,
        subagent: 0,
    };
    // 预算 7 行 = 固定 6 行（摘要 / 空行 / 段标题 / 段间空行 / 收尾空行 / 提示）+ 1 个任务行
    let narrow = TasksViewport::for_content_rows(7, counts, 0);
    assert_eq!(
        narrow.rows(),
        &[
            TasksRow::Summary,
            TasksRow::Blank,
            TasksRow::SectionHeader(TaskSegment::Background),
            TasksRow::Item(0),
            TasksRow::Blank,
            TasksRow::NavHint,
        ],
        "预算只放得下一项时窗口只渲染一项"
    );
    assert_eq!(narrow.item_at_row(2), None, "段标题行不可命中");

    // 预算 3 行连一个任务行都放不下：行表截断，任何行都不可命中
    let tiny = TasksViewport::for_content_rows(3, counts, 0);
    assert_eq!(tiny.rows().len(), 3);
    for row in 0..tiny.rows().len() {
        assert_eq!(tiny.item_at_row(row), None, "第 {row} 行不得命中");
    }
    assert!(
        TasksViewport::for_content_rows(0, counts, 0)
            .rows()
            .is_empty()
    );
}

/// 列表刷新缩短后选中下标收敛到末项，选中行始终有对应的全量下标。
#[test]
fn selection_clamps_within_frame() {
    let counts = SegmentCounts {
        background: 3,
        cron: 1,
        subagent: 0,
    };
    assert_eq!(
        TasksViewport::for_content_rows(16, counts, 9).selected(),
        3,
        "越界选中收敛到末项"
    );
    assert_eq!(
        TasksViewport::for_content_rows(16, SegmentCounts::default(), 5).selected(),
        0,
        "空列表收敛到 0"
    );
    assert_eq!(
        TasksViewport::for_content_rows(16, counts, 8).selected_item(),
        Some(3),
        "选中项有对应的全量下标"
    );
    assert_eq!(
        TasksViewport::for_content_rows(16, SegmentCounts::default(), 0).selected_item(),
        None,
        "空列表没有可执行的行"
    );
    // 面板矮到行表放不下任务行：选中项不可见，Enter 不得取消它
    assert_eq!(
        TasksViewport::for_content_rows(3, counts, 2).selected_item(),
        None,
        "行表里没有的任务行不可执行"
    );
}

/// 取消动作只对后台任务段的行成立；身份取自传入的当帧快照。
#[test]
fn cancel_action_only_for_background_rows() {
    let counts = SegmentCounts {
        background: 2,
        cron: 1,
        subagent: 1,
    };
    let viewport = TasksViewport::for_content_rows(16, counts, 0);
    let snapshot = vec![bg_task("bg0"), bg_task("bg1")];

    assert_eq!(
        viewport.cancel_action(1, &snapshot),
        CancelAction::Cancel(CancelTarget {
            task_id: "bg1".to_string(),
            kind: "shell".to_string(),
        })
    );
    assert_eq!(
        viewport.cancel_action(2, &snapshot),
        CancelAction::NotCancelable,
        "Cron 行不指向后台任务"
    );
    assert_eq!(
        viewport.cancel_action(3, &snapshot),
        CancelAction::NotCancelable,
        "SubAgent 行不指向后台任务"
    );
    assert_eq!(
        viewport.cancel_action(1, &snapshot[..1]),
        CancelAction::TargetGone,
        "快照里没有该身份 → 不发请求"
    );
}
