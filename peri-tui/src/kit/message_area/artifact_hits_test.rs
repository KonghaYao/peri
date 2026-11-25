//! entry 副产物命中几何：两个 surface（主区 / 详情面板）共用同一份列口径。

use super::artifact_hits::{content_area, mermaid_action_columns};
use super::grid::GridSpec;
use ratatui_kit::ratatui::layout::Rect;

/// 面板没有滚动条列：命中区右缘是开区间，最后一列正文仍需可点。
#[test]
fn mermaid_action_columns_cover_last_content_column() {
    let grid = GridSpec::grid_for(80);
    let area = Rect::new(4, 7, 64, 30);
    let (start, end) = mermaid_action_columns(grid, area);
    assert_eq!(start, area.x + grid.cont_prefix_width() as u16);
    assert_eq!(end, area.x + area.width, "右缘是开区间");
    assert!(
        area.x + area.width - 1 < end,
        "最后一列正文必须落在命中区内"
    );
}

/// 主区把含滚动条的整块区域交给命中计算；扣掉滚动条列后行为与
/// 收敛前的 `area.x + area.width - 1` 一致。
#[test]
fn main_area_content_area_excludes_scrollbar_column() {
    let grid = GridSpec::grid_for(80);
    let area = Rect::new(4, 7, 64, 30);
    let content = content_area(area);
    assert_eq!(content.x, area.x);
    assert_eq!(content.y, area.y);
    assert_eq!(content.width, area.width - 1);
    assert_eq!(content.height, area.height);

    let (_, end) = mermaid_action_columns(grid, content);
    assert_eq!(end, area.x + area.width - 1);
}

/// 内容区比正文前缀还窄时把起点钳到右缘，不产生倒挂区间。
#[test]
fn mermaid_action_columns_clamp_prefix_to_narrow_content() {
    let grid = GridSpec::grid_for(80);
    let area = Rect::new(3, 0, 1, 1);
    let (start, end) = mermaid_action_columns(grid, area);
    assert_eq!(end, area.x + area.width);
    assert!(start <= end);
}
