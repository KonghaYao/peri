//! `OverlayFrame` 的标题、边框、内区与边界命中回归。
//!
//! 关键约定：外框只画上下边框和标题，**不清屏**（清屏由宿主在既有位置做一次）；
//! 内区与命中来自同一份几何。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui_kit::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{BORDER_ROWS, OverlayAction, OverlayFrame, OverlayHit, actions, hit};
use crate::button::{ButtonChrome, ButtonState, ButtonTone, button_style};

fn frame() -> OverlayFrame<'static> {
    OverlayFrame::new(Line::from("确认操作"))
        .border(Style::new().fg(Color::DarkGray))
        .title_style(Style::new().fg(Color::Cyan).bold())
}

/// 非零起点：内区与命中都按绝对坐标反推。
fn area() -> Rect {
    Rect::new(10, 5, 24, 8)
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn down(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

/// 跳过双宽字符的续接格，只拼接真实字素（与 storybook dump 同一读法）。
fn row_text(buf: &Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0u16;
    for x in buf.area.x..buf.area.right() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let symbol = buf[(x, y)].symbol();
        text.push_str(symbol);
        skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1) as u16;
    }
    text
}

fn cell(buf: &Buffer, x: u16, y: u16) -> String {
    buf[(x, y)].symbol().to_string()
}

#[test]
fn inner_is_the_area_between_the_two_borders() {
    assert_eq!(BORDER_ROWS, 2);
    assert_eq!(frame().inner(area()), Rect::new(10, 6, 24, 6));
}

#[test]
fn inner_collapses_when_the_area_cannot_hold_both_borders() {
    for height in 0..=BORDER_ROWS {
        let inner = frame().inner(Rect::new(10, 5, 24, height));
        assert_eq!(inner.height, 0, "height {height} 没有可用内区");
        assert_eq!(inner.width, 24);
    }
}

#[test]
fn hit_separates_inside_border_and_outside() {
    let area = area();
    // 上边框 / 下边框
    assert_eq!(hit(&down(11, 5), area), OverlayHit::Border);
    assert_eq!(hit(&down(11, 12), area), OverlayHit::Border);
    // 内区
    assert_eq!(hit(&down(11, 6), area), OverlayHit::Inside);
    assert_eq!(hit(&down(11, 11), area), OverlayHit::Inside);
    // 列/行越界
    assert_eq!(hit(&down(9, 6), area), OverlayHit::Outside);
    assert_eq!(hit(&down(34, 6), area), OverlayHit::Outside);
    assert_eq!(hit(&down(11, 4), area), OverlayHit::Outside);
    assert_eq!(hit(&down(11, 13), area), OverlayHit::Outside);
    // 零尺寸（首帧尚未布局）没有任何命中
    assert_eq!(
        hit(&down(10, 5), Rect::new(10, 5, 0, 0)),
        OverlayHit::Outside
    );
}

#[test]
fn hit_ignores_the_mouse_event_kind() {
    let area = area();
    for kind in [
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
        MouseEventKind::Moved,
        MouseEventKind::Up(MouseButton::Left),
    ] {
        assert_eq!(hit(&mouse(kind, 11, 6), area), OverlayHit::Inside);
        assert_eq!(hit(&mouse(kind, 11, 5), area), OverlayHit::Border);
    }
}

#[test]
fn frame_draws_centered_title_on_the_top_border_and_a_bottom_border() {
    let area = area();
    let mut buf = Buffer::empty(area);
    frame().render(area, &mut buf);

    let top = row_text(&buf, 5);
    // 只有 TOP/BOTTOM 边框：整行横线，没有左右角与左右边框。
    assert_eq!(
        top.chars().filter(|ch| *ch != '─').collect::<String>(),
        "确认操作",
        "标题画在上边框: {top:?}"
    );
    // 24 列、标题 8 列 → 居中起始列 = 10 + 8
    assert_eq!(cell(&buf, 18, 5), "确");
    assert_eq!(cell(&buf, 20, 5), "认");
    assert_eq!(buf[(18, 5)].fg, Color::Cyan, "标题样式来自宿主");

    let bottom = row_text(&buf, 12);
    assert!(
        bottom.chars().all(|ch| ch == '─'),
        "底行是整行横线且没有标题: {bottom:?}"
    );

    // 上下边框之间没有左右边框
    for y in 6..=11 {
        let row = row_text(&buf, y);
        assert!(
            row.chars().all(|ch| ch == ' '),
            "第 {y} 行不应绘制内容: {row:?}"
        );
    }
}

#[test]
fn frame_does_not_clear_the_inner_area() {
    // 宿主在既有位置只清屏一次；外框不得覆盖内区或外区已画好的内容。
    let area = area();
    let mut buf = Buffer::empty(area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buf[(x, y)].set_symbol("x");
        }
    }
    frame().render(area, &mut buf);

    for y in 6..=11 {
        for x in area.x..area.right() {
            assert_eq!(cell(&buf, x, y), "x", "内区 ({x},{y}) 被外框清掉");
        }
    }
    // 外框之外同样不受影响
    let outside = Rect::new(0, 0, 60, 20);
    let mut buf = Buffer::empty(outside);
    for y in 0..20 {
        for x in 0..60 {
            buf[(x, y)].set_symbol("y");
        }
    }
    frame().render(area, &mut buf);
    assert_eq!(cell(&buf, 0, 0), "y");
    assert_eq!(cell(&buf, 59, 19), "y");
}

#[test]
fn title_and_border_styles_stay_host_owned() {
    let area = Rect::new(0, 0, 20, 3);
    let mut buf = Buffer::empty(area);
    OverlayFrame::new(Line::from("T"))
        .border(Style::new().fg(Color::Red))
        .title_style(Style::new().fg(Color::Green))
        .render(area, &mut buf);
    let title = (0..20)
        .find(|x| buf[(*x, 0)].symbol() == "T")
        .expect("title");
    assert_eq!(buf[(title, 0)].fg, Color::Green);
    assert_eq!(buf[(0, 0)].fg, Color::Red, "边框颜色由宿主传入");
}

#[test]
fn actions_render_the_shared_filled_buttons_with_their_hit_columns() {
    let theme = peri_theme::builtin::dark_theme();
    let row = actions("确认", "返回", &theme);

    // 通用 Button 的 Filled 文本（两侧各一列内边距）+ 两列缩进与两列间隔。
    // 「确认」为双宽字符，按钮实际占 6 列。
    assert_eq!(row.line.to_string(), "   确认    返回 ");
    assert_eq!(row.confirm, (2, 8));
    assert_eq!(row.back, (10, 16));
    assert_eq!(row.action_at(1), None);
    assert_eq!(row.action_at(2), Some(OverlayAction::Confirm));
    assert_eq!(row.action_at(7), Some(OverlayAction::Confirm));
    assert_eq!(row.action_at(8), None);
    assert_eq!(row.action_at(9), None);
    assert_eq!(row.action_at(10), Some(OverlayAction::Back));
    assert_eq!(row.action_at(15), Some(OverlayAction::Back));
    assert_eq!(row.action_at(16), None);
}

#[test]
fn action_buttons_reuse_the_general_button_tones() {
    let theme = peri_theme::builtin::dark_theme();
    let row = actions("确认", "返回", &theme);
    let span_for = |label: &str| {
        row.line
            .spans
            .iter()
            .find(|span| span.content.contains(label))
            .unwrap_or_else(|| panic!("{label} span"))
    };

    assert_eq!(
        span_for("确认").style,
        button_style(
            ButtonChrome::Filled,
            ButtonTone::Primary,
            ButtonState::Normal,
            &theme
        ),
        "确认按钮与通用 Button 的主按钮样式同源"
    );
    assert_eq!(
        span_for("返回").style,
        button_style(
            ButtonChrome::Filled,
            ButtonTone::Secondary,
            ButtonState::Normal,
            &theme
        ),
        "返回按钮与通用 Button 的次按钮样式同源"
    );
}
