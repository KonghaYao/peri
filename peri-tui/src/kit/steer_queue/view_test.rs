use super::view::{QueueFrame, QueueView, Selection, ViewState, preview, validated_action};
use super::{SteerItemState, SteerQueueAction, SteerQueueItem};
use crate::kit::terminal_caps::{TerminalCaps, symbols};
use peri_theme::prelude::dark_theme;
use ratatui_kit::ratatui::{layout::Rect, style::Style};
use std::sync::{Arc, Mutex};
use unicode_width::UnicodeWidthStr;

fn make_items() -> Vec<SteerQueueItem> {
    ('A'..='G')
        .map(|id| SteerQueueItem {
            id: id.to_string(),
            text: format!("中文内容 {id}"),
            state: SteerItemState::Queued,
        })
        .collect()
}

fn make_view(items: Vec<SteerQueueItem>) -> QueueView {
    QueueView {
        items,
        max_rows: 5,
        state: ViewState::default(),
        symbols: symbols(&TerminalCaps::default()),
        semantic: dark_theme().semantic,
        separator_style: Style::default().fg(dark_theme().component.input.border),
        title: "待发送 7".to_owned(),
        frame: Arc::new(Mutex::new(QueueFrame::default())),
    }
}

#[test]
fn test_queue_layout_empty_has_no_regions() {
    // 空队列连标题及交互区域一起隐藏。
    let frame = make_view(Vec::new()).layout(Rect::new(0, 0, 80, 7));
    assert!(frame.area.is_empty(), "空队列不得保留区域");
    assert!(frame.controls.is_empty(), "空队列不得保留动作");
}

#[test]
fn test_queue_layout_collapsed_all_contains_hidden_ids() {
    // 折叠只影响可见行，全发仍捕获所有确认条目。
    let frame = make_view(make_items()).layout(Rect::new(2, 10, 80, 7));
    assert_eq!(frame.rows.len(), 5, "默认只显示五条");
    assert_eq!(
        frame.dispatch_ids,
        ["A", "B", "C", "D", "E", "F", "G"],
        "全发必须包含隐藏条目"
    );
}

#[test]
fn test_queue_layout_mouse_target_uses_stable_id() {
    // 中文不改变固定行尾动作列，也不把正文点击误认作发送。
    let frame = make_view(make_items()).layout(Rect::new(2, 10, 80, 7));
    assert_eq!(
        frame.hit(76, 13).map(|hit| &hit.selection),
        Some(&Selection::Dispatch("B".to_owned())),
        "第二行发送应绑定 B"
    );
    assert_eq!(
        frame.hit(80, 13).map(|hit| &hit.selection),
        Some(&Selection::TakeBack("B".to_owned())),
        "第二行取回应绑定 B"
    );
    assert!(frame.hit(5, 13).is_none(), "正文不能触发发送");
    assert!(frame.hit(76, 10).is_none(), "上边线不能触发发送");
}

#[test]
fn test_queue_layout_expanded_scroll_keeps_correct_identity() {
    // 实际终端高度裁剪后，命中仍跟随渲染使用的窗口。
    let mut view = make_view(make_items());
    view.state.expanded = true;
    view.state.offset = 4;
    let frame = view.layout(Rect::new(0, 10, 80, 5));
    assert_eq!(
        frame
            .rows
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["E", "F", "G"],
        "展开窗口应从 E 开始"
    );
    assert_eq!(
        frame.hit(74, 13).map(|hit| &hit.selection),
        Some(&Selection::Dispatch("F".to_owned())),
        "滚动后应发送可见 F"
    );
}

#[test]
fn test_queue_layout_queued_items_keep_takeback_enabled() {
    // 撤回动作只依赖队列项状态；编辑器是否恢复由宿主决定。
    let view = make_view(make_items());
    let frame = view.layout(Rect::new(0, 0, 80, 7));
    assert!(
        frame
            .controls
            .iter()
            .filter(|control| matches!(control.selection, Selection::TakeBack(_)))
            .all(|control| control.enabled),
        "每个已排队条目都应允许取回"
    );
    assert!(
        frame
            .controls
            .iter()
            .filter(|control| matches!(control.selection, Selection::Dispatch(_)))
            .all(|control| control.enabled),
        "普通发送仍然可用"
    );
}

#[test]
fn test_queue_action_single_dispatch_only_selected_id() {
    let action = validated_action(
        &Selection::Dispatch("B".to_owned()),
        &["A".to_owned(), "B".to_owned()],
        &make_items(),
    );
    assert_eq!(
        action,
        Some(SteerQueueAction::Dispatch {
            ids: vec!["B".to_owned()]
        }),
        "单发不得带走相邻条目"
    );
}

#[test]
fn test_queue_action_all_keeps_completed_frame_selection() {
    // 新增条目不加入旧帧全发；已失效项跳过。
    let mut items = make_items();
    items[1].state = SteerItemState::Dispatching;
    let action = validated_action(
        &Selection::All,
        &["A".to_owned(), "B".to_owned(), "C".to_owned()],
        &items,
    );
    assert_eq!(
        action,
        Some(SteerQueueAction::Dispatch {
            ids: vec!["A".to_owned(), "C".to_owned()]
        }),
        "全发应保留选择快照并排除失效项"
    );
}

#[test]
fn test_queue_action_takeback_uses_queued_stable_id() {
    let action = validated_action(&Selection::TakeBack("B".to_owned()), &[], &make_items());
    assert_eq!(
        action,
        Some(SteerQueueAction::TakeBack { id: "B".to_owned() }),
        "取回意图只携带选中条目身份"
    );
}

#[test]
fn test_queue_action_transitional_items_reject_commands() {
    let mut items = make_items();
    items[0].state = SteerItemState::Submitting;
    items[1].state = SteerItemState::Dispatching;
    items[2].state = SteerItemState::Withdrawing;
    for id in ["A", "B", "C"] {
        assert_eq!(
            validated_action(&Selection::Dispatch(id.to_owned()), &[], &items),
            None,
            "暂态不得重复发送 {id}"
        );
        assert_eq!(
            validated_action(&Selection::TakeBack(id.to_owned()), &[], &items),
            None,
            "暂态不得取回 {id}"
        );
    }
}

#[test]
fn test_queue_preview_preserves_display_width_and_graphemes() {
    let text = "你好\n e\u{301} 继续完整正文";
    let shortened = preview(text, 8);
    assert_eq!(shortened, "你好 e\u{301} …", "摘要需单行并保留组合字符");
    assert!(shortened.width() <= 8, "省略号也必须计入显示宽度");
    assert_eq!(preview(text, 0), "", "零列不得溢出");
}

#[test]
fn test_queue_layout_narrow_controls_do_not_overlap() {
    let frame = make_view(make_items()).layout(Rect::new(0, 0, 4, 7));
    for (index, control) in frame.controls.iter().enumerate() {
        for other in frame.controls.iter().skip(index + 1) {
            assert!(
                control.area.intersection(other.area).is_empty(),
                "窄区域动作不可重叠"
            );
        }
    }
}

#[test]
fn test_queue_keyboard_navigation_includes_takeback_for_each_queued_item() {
    let items = make_items();
    let targets = super::view::selections(&items, 5);
    for item in items {
        assert!(
            targets.contains(&Selection::TakeBack(item.id)),
            "取回应始终可以通过显式键盘导航到达"
        );
    }
}
