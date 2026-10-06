use super::*;

// ── [Slice 4 §6.8] Interaction block 渲染 ──

/// pending 态：标题（`Approval required`）+ 问题摘要 + 横向选项行 + 布局信息。
#[test]
fn test_interaction_pending_permission_layout() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(pending_permission_block());
    let grid = GridSpec::grid_for(80); // Standard，横向选项
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Approval required"), "标题行应含 FTL 文案");
    assert!(
        text.contains("Bash wants to run: cargo test"),
        "问题摘要行（人类摘要）"
    );
    assert!(
        text.contains("[Allow once]  [Deny]"),
        "横向选项行（§6.8 视觉）"
    );

    let layout = layout.expect("pending 态必须返回选项布局");
    assert_eq!(layout.option_rows.len(), 1, "横向选项共享一行");
    assert_eq!(layout.option_cols.len(), 2, "每选项一个列区间");
    // 列区间：第二选项起始列 > 第一选项
    let (s0, e0) = layout.option_cols[0].expect("宽屏下第一选项有列区间");
    let (s1, _e1) = layout.option_cols[1].expect("宽屏下第二选项有列区间");
    assert!(s0 < s1 && e0 <= s1, "选项列区间不重叠且有序");
}

/// Narrow 断点（§11）：interaction 选项垂直排列（每行一个），整行命中。
#[test]
fn test_interaction_pending_narrow_vertical_options() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(pending_permission_block());
    let grid = GridSpec::grid_for(30); // Narrow
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("[Allow once]"), "垂直排列第一项");
    assert!(text.contains("[Deny]"), "垂直排列第二项");
    // 不在同一行（垂直）→ 不包含双选项拼接文本
    assert!(!text.contains("[Allow once]  [Deny]"));

    let layout = layout.expect("pending 态必须返回选项布局");
    assert_eq!(layout.option_rows.len(), 2);
    assert_ne!(layout.option_rows[0], layout.option_rows[1], "垂直分行");
    assert_eq!(
        layout.option_cols[0], None,
        "Narrow 垂直排列整行命中（无列区间）"
    );
}

/// completed + 手动折叠（Space → Collapsed）：单行结果（`✓ Allowed once`
/// 风格：符号 + result）——自动策略已是 Expanded，Collapsed 只能来自用户覆盖。
#[test]
fn test_interaction_completed_single_line_result() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(completed_block("Allowed once"));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Allowed once"), "结果行含 result 文案");
    assert!(
        !text.contains("Bash wants to run"),
        "Collapsed 单行不显示问题摘要"
    );
    assert!(layout.is_none(), "completed 无选项布局");
    // 单行
    assert_eq!(lines.len(), 1, "Collapsed 收束为单行");
}

/// completed + 展开（用户需求：答毕默认完整展示）：结果行 + 问题摘要 +
/// 选项行（选中项 ✓ 标记）。
#[test]
fn test_interaction_completed_expanded_shows_question() {
    crate::i18n::init(Some("en"));
    let mut b = completed_block("Deny"); // result 与选项 "Deny" 精确匹配 → 选中标记
    b.fold = FoldState::Expanded;
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Deny"), "结果行");
    assert!(
        text.contains("Bash wants to run: cargo test"),
        "展开时问题摘要可见"
    );
    // [用户需求] completed 展开态也显示选项，选中项 ✓ 标记（result 匹配项）
    assert!(text.contains("[✓ Deny]"), "选中项 ✓ 标记，实际 {text:?}");
    assert!(text.contains("[Allow once]"), "未选项仍可见，实际 {text:?}");
}
