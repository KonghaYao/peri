use super::*;

#[test]
fn directory_suffix_keeps_workspace_name_and_unicode_width() {
    assert_eq!(path_tail("/long/仓库/feature", 12), "…库/feature");
    assert_eq!(path_tail("/a", 10), "/a");
    assert_eq!(path_tail("/a", 0), "");
}

#[test]
fn truncation_boundaries_are_display_width_safe() {
    assert_eq!(truncate_text("abcdef", 0), "");
    assert_eq!(truncate_text("abcdef", 1), "…");
    assert_eq!(truncate_text("abcdef", 2), "a…");
    assert_eq!(truncate_text("abcdef", 3), "ab…");
    assert_eq!(truncate_text("仓库名称", 1), "…");
    assert_eq!(truncate_text("仓库名称", 2), "…");
    assert_eq!(truncate_text("仓库名称", 3), "仓…");
    assert_eq!(truncate_text("仓库名称", 4), "仓…");
    assert_eq!(truncate_text("仓库名称", 5), "仓库…");
    assert_eq!(truncate_text("仓库名称", 8), "仓库名称");
    assert_eq!(truncate_text("", 1), "");
    assert_eq!(truncate_text("a", 1), "a");
    assert_eq!(truncate_text("a", 2), "a");
    assert_eq!(truncate_text("界", 2), "界");
    assert_eq!(truncate_text("界面", 3), "界…");
    assert_eq!(truncate_text("abcdef", 4), "abc…");
    assert_eq!(truncate_text("abcdef", 5), "abcd…");
    assert_eq!(truncate_text("abcdef", 6), "abcdef");
    assert_eq!(truncate_text("中文", 4), "中文");
    assert_eq!(truncate_text("中文测试", 6), "中文…");
    assert_eq!(truncate_text("中文测试", 7), "中文测…");
    assert_eq!(truncate_text("中文测试", 8), "中文测试");
    assert_eq!(truncate_text("中文测试", 9), "中文测试");
    assert_eq!(truncate_text("x", 0), "");
    assert_eq!(truncate_text("x", 2), "x");
    assert_eq!(truncate_text("xy", 2), "xy");
}

#[test]
fn selection_visibility_clamps_to_resized_viewport() {
    let mut state = ratatui_kit::components::scroll_view::ScrollViewState::with_offset(
        ratatui_kit::ratatui::layout::Position::new(0, 10),
    );
    keep_selection_visible(&mut state, 3, 20, 5);
    assert_eq!(state.offset().y, 3);
    keep_selection_visible(&mut state, 19, 20, 5);
    assert_eq!(state.offset().y, 15);
    keep_selection_visible(&mut state, 0, 0, 5);
    assert_eq!(state.offset().y, 15);
}
