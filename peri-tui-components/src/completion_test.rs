//! `CompletionPopup` 的窗口、命中与样式回归。
//!
//! 关键约定：同一 `CompletionPopup` 的 `widget()` 与 `hit_layout()` 必须给出
//! 一致的行；候选变化或高度变化后，点击命中的全量下标仍对应绘制出的那一行。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui_kit::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{CompletionPopup, CompletionRow, CompletionStyles};

const WIDTH: u16 = 40;
/// 非零起点：命中必须按 area 的绝对坐标反推，而不是假设从 0 开始。
const TOP: u16 = 10;

fn styles() -> CompletionStyles {
    CompletionStyles {
        border: Color::Blue,
        selected_label: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        selected_detail: Style::new().fg(Color::Cyan),
        empty: Style::new().fg(Color::DarkGray),
    }
}

fn row(index: usize, detail: bool) -> CompletionRow<'static> {
    let label = Line::from(format!("item-{index:02}"));
    if detail {
        CompletionRow::new(label).with_detail(Line::from(format!("detail-{index:02}")))
    } else {
        CompletionRow::new(label)
    }
}

fn rows(count: usize, detail: bool) -> Vec<CompletionRow<'static>> {
    (0..count).map(|index| row(index, detail)).collect()
}

fn popup(
    rows: Vec<CompletionRow<'static>>,
    selected: usize,
    height: u16,
) -> CompletionPopup<'static> {
    CompletionPopup {
        rows,
        selected,
        height,
        title: Line::from(" completion "),
        empty_text: Line::from("no matches"),
        styles: styles(),
    }
}

fn area(height: u16) -> Rect {
    Rect::new(0, TOP, WIDTH, height)
}

fn render(popup: &CompletionPopup<'_>, height: u16) -> Buffer {
    let area = area(height);
    let mut buffer = Buffer::empty(area);
    popup.widget().render(area, &mut buffer);
    buffer
}

fn click(row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: TOP + row,
        modifiers: KeyModifiers::NONE,
    }
}

fn painted(buffer: &Buffer, row: u16) -> String {
    let mut text = String::new();
    for x in 0..WIDTH {
        text.push_str(buffer[(x, TOP + row)].symbol());
    }
    text.trim().to_string()
}

#[test]
fn painted_rows_and_hit_rows_share_one_window() {
    let popup = popup(rows(20, false), 14, 8);
    let buffer = render(&popup, 8);
    let layout = popup.hit_layout();

    // 上下边框不可点
    assert_eq!(popup.hit(&click(0), area(8)), None);
    assert_eq!(popup.hit(&click(7), area(8)), None);

    // 内容行 6 行：绘制文本与命中下标必须来自同一窗口
    for visual in 0..layout.visible_items {
        let mouse_row = visual + 1;
        let index = popup
            .hit(&click(mouse_row), area(8))
            .expect("content row is clickable");
        assert_eq!(index, layout.scroll_start + visual as usize);
        assert!(
            painted(&buffer, mouse_row).ends_with(&format!("item-{index:02}")),
            "row {mouse_row} painted {:?} but hit index {index}",
            painted(&buffer, mouse_row)
        );
    }
}

#[test]
fn scroll_window_returns_absolute_index() {
    let popup = popup(rows(50, false), 30, 7);
    let layout = popup.hit_layout();
    assert_eq!(layout.visible_items, 5);
    // 选中项保持在视口上 1/3：30 - 5/3 = 29
    assert_eq!(layout.scroll_start, 29);

    assert_eq!(popup.hit(&click(1), area(7)), Some(29));
    assert_eq!(popup.hit(&click(2), area(7)), Some(30));
    assert_eq!(popup.hit(&click(5), area(7)), Some(33));
    assert_eq!(popup.hit(&click(6), area(7)), None);
}

#[test]
fn resize_keeps_paint_and_hit_in_sync() {
    for height in [5u16, 8, 12] {
        let popup = popup(rows(20, false), 19, height);
        let buffer = render(&popup, height);
        let layout = popup.hit_layout();
        assert_eq!(layout.visible_items, height.saturating_sub(2));
        for visual in 0..height - 2 {
            let mouse_row = visual + 1;
            match popup.hit(&click(mouse_row), area(height)) {
                Some(index) => {
                    assert_eq!(index, layout.scroll_start + visual as usize);
                    assert!(
                        painted(&buffer, mouse_row).ends_with(&format!("item-{index:02}")),
                        "height {height}: painted row {mouse_row} does not match hit index {index}"
                    );
                }
                None => assert!(
                    painted(&buffer, mouse_row).is_empty(),
                    "height {height}: unpainted row {mouse_row} must not be clickable"
                ),
            }
        }
    }
}

#[test]
fn shrinking_candidates_clamps_selection_and_keeps_hit_in_range() {
    let before = popup(rows(20, false), 19, 8);
    assert_eq!(before.selected(), 19);

    let after = popup(rows(3, false), 19, 8);
    assert_eq!(after.selected(), 2);
    let buffer = render(&after, 8);
    assert!(painted(&buffer, 3).contains("item-02"));
    assert!(painted(&buffer, 3).starts_with("> "));

    for visual in 0..6u16 {
        if let Some(index) = after.hit(&click(visual + 1), area(8)) {
            assert!(index < 3);
            assert!(painted(&buffer, visual + 1).ends_with(&format!("item-{index:02}")));
        }
    }
}

#[test]
fn selected_row_uses_label_and_detail_styles_separately() {
    let popup = popup(vec![row(3, true), row(4, true)], 0, 5);
    let buffer = render(&popup, 5);

    // 选中行：marker + label 用 selected_label（含 BOLD），描述列用 selected_detail（不加粗）
    let label_cell = buffer[(2, TOP + 1)].style();
    assert_eq!(label_cell.fg, Some(Color::Cyan));
    assert!(label_cell.add_modifier.contains(Modifier::BOLD));

    let detail_x = 2 + "item-03".len() as u16 + 2;
    let detail_cell = buffer[(detail_x, TOP + 1)].style();
    assert_eq!(detail_cell.fg, Some(Color::Cyan));
    assert!(!detail_cell.add_modifier.contains(Modifier::BOLD));

    // 未选中行不套选中色
    let normal_cell = buffer[(2, TOP + 2)].style();
    assert_ne!(normal_cell.fg, Some(Color::Cyan));
}

#[test]
fn empty_popup_paints_no_matches_and_never_hits() {
    let popup = popup(Vec::new(), 3, 5);
    let buffer = render(&popup, 5);
    assert!(painted(&buffer, 1).contains("no matches"));
    assert_eq!(popup.selected(), 0);
    for row in 0..5 {
        assert_eq!(popup.hit(&click(row), area(5)), None);
    }
}

#[test]
fn tiny_heights_do_not_panic() {
    for height in [0u16, 1, 2, 3] {
        let popup = popup(rows(5, true), 4, height);
        let buffer = render(&popup, height);
        let layout = popup.hit_layout();
        assert_eq!(layout.visible_items, height.saturating_sub(2));
        assert_eq!(
            popup.hit(&click(0), area(height)),
            None,
            "height {height} must not hit the border"
        );
        // 3 行高时内容区只有 1 行，末项滚到可见窗口仍然一致
        if height == 3 {
            assert_eq!(popup.hit(&click(1), area(3)), Some(4));
            assert!(painted(&buffer, 1).contains("item-04"));
        }
    }
}

#[test]
fn wheel_is_not_a_hit() {
    let popup = popup(rows(5, false), 0, 5);
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: TOP + 1,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(popup.hit(&wheel, area(5)), None);
}
