//! 面板/弹窗鼠标点击辅助——统一「click as enter」命中测试。
//!
//! 鼠标事件过滤由 ratatui-kit 框架完成：组件用
//! `use_event_handler_with_options(scope, priority, EventOptions { hit_test: true }, ..)`
//! 注册后，框架按组件自身绘制区域（上一帧 `pre_component_draw` 回填）自动过滤，
//! 区域外的鼠标事件不会进入闭包（`input/mod.rs::call_handler`）。
//!
//! 本模块只负责把命中坐标反推为列表项索引。面板/弹窗内容统一渲染在
//! 组件 area 顶部边框（1 行）之下（`panel_shell!` / `popup_text_shell!` 的 TOP border），
//! 因此内容行号 = `mouse.row - (area.y + 1)`。

use ratatui_kit::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

/// 追踪组件绘制区域——事件闭包行号反推所需。
///
/// 值拷贝模式（仿 `message_area/props.rs::MsgAreaTracker` 与
/// `input_area.rs::AreaTracker`）：Hook 跨帧持久，`pre_component_draw` 每帧
/// 回填上一帧的 `drawer.area`；渲染体读取副本（`Option<Rect>` 是 Copy），
/// 事件闭包按值捕获。区域命中过滤本身由 `hit_test` 完成，这里只提供
/// 行号反推所需的绝对坐标。
pub struct AreaTracker {
    pub rect: Option<Rect>,
}

impl AreaTracker {
    pub fn new() -> Self {
        Self { rect: None }
    }
}

impl Default for AreaTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Hook for AreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

/// 固定行布局列表的命中契约——由每个面板在渲染体按自身布局提供。
///
/// 内容区行号 0 起：前 `header_rows` 行为 header（不可点），其后每 `item_rows`
/// 行对应一个列表项（项内任意行都命中该项），再后 `footer_rows` 行（不可点）。
/// `scroll_start` 是渲染时 `skip(scroll_start).take(visible_items)` 裁剪掉的项数，
/// 命中索引需要加回；footer 起始行 = header + 实际渲染项数 × item_rows。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListLayout {
    /// 内容区中 header 占的行数（不可点击）
    pub header_rows: u16,
    /// 每个列表项占的行数（固定布局）
    pub item_rows: u16,
    /// footer 占的行数（不可点击）
    pub footer_rows: u16,
    /// 渲染时视口最多容纳的项数（`take(visible_items)`）
    pub visible_items: u16,
    /// 渲染时裁剪掉的项数（视口跟随滚动偏移）
    pub scroll_start: usize,
    /// 全量列表项数
    pub item_count: usize,
}

/// 左键 Down 事件的位置。其他鼠标事件（Drag/Up/Scroll/Moved…）返回 None。
pub fn left_down(mouse: &MouseEvent) -> Option<(u16, u16)> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Some((mouse.row, mouse.column)),
        _ => None,
    }
}

/// 判断鼠标列是否落在最右滚动条列（ScrollView 渲染滚动条时该列不可点）。
pub fn is_scrollbar_column(mouse: &MouseEvent, area: Rect) -> bool {
    mouse.column == area.x.saturating_add(area.width).saturating_sub(1)
}

/// 鼠标左键点击 → 列表项索引（相对全量列表）。
///
/// - 顶部边框行、header/footer 行 → None
/// - 命中项内部任意一行（如 title 行或 meta 行）都视为命中该项
/// - 调用方需自行用 `is_scrollbar_column` 排除滚动条列
pub fn hit_item(mouse: &MouseEvent, area: Rect, layout: ListLayout) -> Option<usize> {
    let (row, _col) = left_down(mouse)?;
    hit_row(row, area, layout)
}

/// 按鼠标行号反推列表项索引（不含列检查）。
pub fn hit_row(mouse_row: u16, area: Rect, layout: ListLayout) -> Option<usize> {
    // 顶部边框行不可点
    if mouse_row < area.y.saturating_add(1) {
        return None;
    }
    let visual = mouse_row.saturating_sub(area.y).saturating_sub(1);
    if visual < layout.header_rows {
        return None;
    }
    // 视口外（含底部边框行）不可点
    let content_height = area.height.saturating_sub(2);
    if visual >= content_height {
        return None;
    }
    // footer 起始行 = header + 实际渲染项数 × item_rows；
    // 内容不满视口时 footer 在 footer_start 处（不可点），
    // 内容超视口时 footer 不可见，clamp 到视口底（全部可命中）。
    let rendered = (layout.item_count.saturating_sub(layout.scroll_start))
        .min(layout.visible_items as usize) as u16;
    let footer_start = layout
        .header_rows
        .saturating_add(rendered.saturating_mul(layout.item_rows))
        .min(content_height);
    if visual >= footer_start {
        return None;
    }
    if layout.item_rows == 0 {
        return None;
    }
    let in_view = (visual - layout.header_rows) / layout.item_rows;
    let idx = (layout.scroll_start as u16).saturating_add(in_view) as usize;
    (idx < layout.item_count).then_some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::crossterm::event::MouseEvent;

    fn area() -> Rect {
        Rect::new(10, 20, 60, 14)
    }

    fn down(row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: ratatui_kit::crossterm::event::KeyModifiers::NONE,
        }
    }

    const LAYOUT: ListLayout = ListLayout {
        header_rows: 3,
        item_rows: 3,
        footer_rows: 1,
        visible_items: 4,
        scroll_start: 0,
        item_count: 8,
    };

    #[test]
    fn top_border_not_hit() {
        // area.y 是顶部边框行
        assert_eq!(hit_item(&down(20, 30), area(), LAYOUT), None);
    }

    #[test]
    fn header_not_hit() {
        // 内容区 0..3 行是 header（area.y+1 = 21 起）
        assert_eq!(hit_item(&down(21, 30), area(), LAYOUT), None);
        assert_eq!(hit_item(&down(23, 30), area(), LAYOUT), None);
    }

    #[test]
    fn item_any_row_hits() {
        // 内容行 3,4,5 → item 0
        assert_eq!(hit_item(&down(24, 30), area(), LAYOUT), Some(0));
        assert_eq!(hit_item(&down(25, 30), area(), LAYOUT), Some(0));
        assert_eq!(hit_item(&down(26, 30), area(), LAYOUT), Some(0));
        // 内容行 6 → item 1
        assert_eq!(hit_item(&down(27, 30), area(), LAYOUT), Some(1));
    }

    #[test]
    fn scroll_start_offset() {
        let mut l = LAYOUT;
        l.scroll_start = 2;
        // 内容行 3..5 → 全量 item 2
        assert_eq!(hit_item(&down(24, 30), area(), l), Some(2));
        assert_eq!(hit_item(&down(26, 30), area(), l), Some(2));
    }

    #[test]
    fn viewport_bottom_and_bottom_border_not_hit() {
        // 内容 15 行（3 header + 4×3 items）超视口 12 行，footer 不可见：
        // 视口内内容行 3..11 全部可命中 item 0-2，内容行 12+ 不可见 → None
        assert_eq!(
            hit_item(&down(area().y + 1 + 11, 30), area(), LAYOUT),
            Some(2)
        );
        // 内容行 12（在视口外，也是底部边框行上方）→ None
        assert_eq!(hit_item(&down(area().y + 1 + 12, 30), area(), LAYOUT), None);
        // 底部边框行
        assert_eq!(
            hit_item(&down(area().y + area().height - 1, 30), area(), LAYOUT),
            None
        );
    }

    #[test]
    fn content_shorter_than_viewport_footer_not_hit() {
        // 内容 12 行（2 header + 8×1 items + 2 footer）≤ 视口 12 行：
        // footer 从内容行 10 起，点击 footer 行 → None
        let l = ListLayout {
            header_rows: 2,
            item_rows: 1,
            footer_rows: 2,
            visible_items: 8,
            scroll_start: 0,
            item_count: 8,
        };
        assert_eq!(hit_item(&down(area().y + 1 + 9, 30), area(), l), Some(7));
        assert_eq!(hit_item(&down(area().y + 1 + 10, 30), area(), l), None);
        assert_eq!(hit_item(&down(area().y + 1 + 11, 30), area(), l), None);
    }

    #[test]
    fn scroll_to_bottom_all_visible_hit() {
        let mut l = LAYOUT;
        l.scroll_start = 4;
        l.item_count = 8;
        // 渲染 4 项（8-4=4 ≤ visible 4）超视口 12 行 → footer 不可见，
        // 视口内内容行 3..11 命中全量 item 4..6
        assert_eq!(hit_item(&down(area().y + 1 + 3, 30), area(), l), Some(4));
        assert_eq!(hit_item(&down(area().y + 1 + 11, 30), area(), l), Some(6));
        // 内容行 12+ → None
        assert_eq!(hit_item(&down(area().y + 1 + 12, 30), area(), l), None);
    }

    #[test]
    fn out_of_area_columns() {
        // 列检查由 hit_test 负责；这里列不影响结果（模块不检查列）
        assert_eq!(hit_item(&down(24, 10), area(), LAYOUT), Some(0));
    }

    #[test]
    fn not_left_click() {
        let m = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 30,
            row: 24,
            modifiers: ratatui_kit::crossterm::event::KeyModifiers::NONE,
        };
        assert_eq!(hit_item(&m, area(), LAYOUT), None);
        assert_eq!(left_down(&m), None);
    }

    #[test]
    fn scrollbar_column_detection() {
        let m = down(24, area().x + area().width - 1);
        assert!(is_scrollbar_column(&m, area()));
        let m2 = down(24, area().x + area().width - 2);
        assert!(!is_scrollbar_column(&m2, area()));
    }

    #[test]
    fn empty_list() {
        let l = ListLayout {
            item_count: 0,
            ..LAYOUT
        };
        assert_eq!(hit_item(&down(24, 30), area(), l), None);
    }

    #[test]
    fn single_row_items() {
        let l = ListLayout {
            header_rows: 2,
            item_rows: 1,
            footer_rows: 2,
            visible_items: 13,
            scroll_start: 0,
            item_count: 5,
        };
        // 内容行 2 → item 0；内容行 3 → item 1
        assert_eq!(hit_item(&down(area().y + 1 + 2, 30), area(), l), Some(0));
        assert_eq!(hit_item(&down(area().y + 1 + 3, 30), area(), l), Some(1));
        // 内容行 7（footer 起）→ None
        assert_eq!(hit_item(&down(area().y + 1 + 7, 30), area(), l), None);
    }
}
