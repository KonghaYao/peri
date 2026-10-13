//! 列表项间距（列表项之间插入空行）的渲染与策略测试。

use super::list::{LIST_ITEM_GAP_MAX_DEPTH, list_item_gap};
use super::*;

/// 把 markdown 渲染为纯文本行（空行渲染为空字符串）。
fn render(md: &str) -> Vec<String> {
    parse_markdown(md, 80, Palette::default(), Color::White)
        .iter()
        .flat_map(|segment| match segment {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => Vec::new(),
            MarkdownSegment::Image(image) => image.lines.clone(),
        })
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// 按**生效策略**生成期望渲染结果：连续列表项之间，策略判定该深度插空行时
/// 补一个空行。策略调整（[`LIST_ITEM_GAP_MAX_DEPTH`]）时期望值随之更新，
/// 测试仍校验「渲染结果 == 生效策略」的接线是否正确。
fn expected_list(items: &[(&str, u32)]) -> Vec<String> {
    let mut out = Vec::new();
    for (index, (text, depth)) in items.iter().enumerate() {
        if index > 0 && list_item_gap(LIST_ITEM_GAP_MAX_DEPTH, *depth) {
            out.push(String::new());
        }
        out.push((*text).to_string());
    }
    out
}

/// 生效策略是编译期常量：`scope` 变更（仅顶层 → 所有层级 / 关闭）时，
/// 本文件中的渲染期望会随之变化，须一并复核。
#[test]
fn test_active_list_item_spacing_scope_is_top_level_only() {
    assert_eq!(
        LIST_ITEM_GAP_MAX_DEPTH,
        Some(0),
        "默认范围：仅顶层列表项之间插空行；变更 scope 须同步本文件渲染期望"
    );
}

#[test]
fn test_list_item_gap_scope_matrix() {
    assert!(list_item_gap(Some(0), 0), "顶层项之间应插空行");
    assert!(!list_item_gap(Some(0), 1), "嵌套层保持紧凑");
    assert!(!list_item_gap(Some(0), 2), "更深层同样保持紧凑");
    assert!(list_item_gap(Some(1), 1), "深度上限 1 时嵌套首层插空行");
    assert!(!list_item_gap(Some(1), 2));
    for depth in [0, 1, 2] {
        assert!(list_item_gap(Some(u32::MAX), depth), "所有层级生效");
        assert!(!list_item_gap(None, depth), "关闭时列表项之间不插空行");
    }
}

/// 无序顶层列表：项之间插空行，首项前与末项后不多余空行。
#[test]
fn test_top_level_unordered_items_separated_by_blank_line() {
    let lines = render("- alpha\n- beta\n- gamma");
    assert_eq!(
        lines,
        expected_list(&[("• alpha", 0), ("• beta", 0), ("• gamma", 0)])
    );
}

/// 有序顶层列表：同样按顶层策略插空行，编号与标记不变。
#[test]
fn test_top_level_ordered_items_separated_by_blank_line() {
    let lines = render("1. one\n2. two\n3. three");
    assert_eq!(
        lines,
        expected_list(&[("1. one", 0), ("2. two", 0), ("3. three", 0)])
    );
}

/// 嵌套列表：父项与其子项、子项彼此之间的关系由缩进表达，不插空行时仍紧凑
/// （生效策略为 AllLevels 时按策略插空行）。
#[test]
fn test_nested_items_follow_active_policy() {
    let lines = render("- parent\n  - child a\n  - child b");
    assert_eq!(
        lines,
        expected_list(&[("• parent", 0), ("  • child a", 1), ("  • child b", 1)])
    );
}

/// 嵌套组结束后回到顶层：新顶层项之前插空行，与父项同层。
#[test]
fn test_return_to_top_level_after_nested_group() {
    let lines = render("- parent\n  - child\n- sibling");
    assert_eq!(
        lines,
        expected_list(&[("• parent", 0), ("  • child", 1), ("• sibling", 0)])
    );
}

/// 列表与相邻段落之间仍恰好一行空行（既有块间规则，不因列表项空行而叠加）。
#[test]
fn test_list_boundaries_with_paragraphs_keep_single_blank_line() {
    let lines = render("intro\n\n- alpha\n- beta\n\noutro");
    assert_eq!(
        lines,
        vec!["intro", "", "• alpha", "", "• beta", "", "outro"]
    );
}

/// 非列表块（标题 / 段落）的间距不受本次变更影响。
#[test]
fn test_non_list_block_spacing_unchanged() {
    let lines = render("# title\ntext\n\nmore");
    assert_eq!(lines, vec!["title", "", "text", "", "more"]);
}
