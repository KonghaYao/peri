use super::FoldState;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render;
use ratatui_kit::ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::TuiRenderUnit;

/// 单行文本（测试断言行首归属）。
fn line_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

// ── [Slice 4 §6.8] Interaction block 渲染 ──

/// pending 态：标题（`Approval required`）+ 问题摘要 + 横向选项行 + 布局信息。
#[test]
fn test_interaction_pending_permission_layout() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(super::pending_permission_block());
    let grid = GridSpec::grid_for(80); // Standard，横向选项
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
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
    let vm = TuiRenderUnit::TuiAskUserBlock(super::pending_permission_block());
    let grid = GridSpec::grid_for(30); // Narrow
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
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
    let vm = TuiRenderUnit::TuiAskUserBlock(super::completed_block("Allowed once"));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
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
    let mut b = super::completed_block("Deny"); // result 与选项 "Deny" 精确匹配 → 选中标记
    b.fold = FoldState::Expanded;
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
    assert!(text.contains("Deny"), "结果行");
    // Permission 保持现状：标题 = result 文案，问题摘要在第二行。
    assert!(
        line_text(&lines[0]).contains("Deny")
            && !line_text(&lines[0]).contains("Bash wants to run"),
        "Permission 首行仍是 result 文案，实际 {:?}",
        line_text(&lines[0])
    );
    assert!(
        text.contains("Bash wants to run: cargo test"),
        "展开时问题摘要可见"
    );
    // [用户需求] completed 展开态也显示选项，选中项 ✓ 标记（result 匹配项）
    assert!(text.contains("[✓ Deny]"), "选中项 ✓ 标记，实际 {text:?}");
    assert!(text.contains("[Allow once]"), "未选项仍可见，实际 {text:?}");
}

/// §6.8/§3 选项列区间（`InteractionLayout.option_cols`）随过程区前缀宽右移：
/// 命中区间等于渲染行里 `[label]` 的实际列区间（不落在缩进空格上）；
/// <60 列前缀回退基准形态（§9）时列区间同样跟随。
#[test]
fn test_interaction_option_cols_follow_process_prefix_width() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(super::pending_permission_block());
    for term in [80u16, 59] {
        let grid = GridSpec::grid_for(term);
        let expected_col = if grid.indent == 0 {
            grid.cont_prefix_width()
        } else {
            grid.process_prefix_width()
        };
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        let (lines, _, layout, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
        let layout = layout.expect("pending 态返回选项布局");
        assert_eq!(layout.option_rows.len(), 1, "term={term}: 横向共享一行");
        let row = layout.option_rows[0];

        // 渲染列：累计前序 span 宽度 + 同 span 内前导文本宽度
        let mut col = 0usize;
        let mut rendered = None;
        for span in &lines[row].spans {
            if let Some(pos) = span.content.find("[Allow once]") {
                rendered = Some(col + span.content[..pos].width());
            }
            col += span.content.width();
        }
        let rendered = rendered.expect("term={term}: 选项行含 [Allow once]");
        assert_eq!(rendered, expected_col, "term={term}: 选项列 = 过程区前缀宽");
        assert_eq!(
            layout.option_cols[0],
            Some((rendered as u16, (rendered + "[Allow once]".width()) as u16)),
            "term={term}: 命中区间与渲染列一致"
        );
        let (second_start, _) = layout.option_cols[1].expect("term={term}: 第二选项可命中");
        assert_eq!(
            second_start as usize,
            rendered + "[Allow once]".width() + 2,
            "term={term}: 选项间 2 空格分隔"
        );
        // ≥60 列：选项已右移，不再落在文本区基准列
        if grid.indent > 0 {
            assert!(rendered > grid.first_prefix_width());
        }
    }
}

// ── [AskUser 显示归属] completed AskUser block：标题=首问 header，答案不漂移 ──

/// completed 展开态：首行 = 状态符号 + 问题 header（不是答案）；预设选项
/// 选中项 ✓，未匹配时不追加 `Selected:` 行。
#[test]
fn test_ask_user_completed_expanded_title_is_header() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(super::completed_ask_user_block("Fast", &["q1"]));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let first = line_text(&lines[0]);
    assert!(
        first.contains("\u{2713}") && first.contains("Length"),
        "首行 = 状态符号 + 问题 header，实际 {first:?}"
    );
    assert!(!first.contains("Fast"), "答案不得作为标题，实际 {first:?}");

    let text = super::all_text(&lines);
    assert!(
        text.contains("[\u{2713} Fast]"),
        "选中预设项 ✓，实际 {text:?}"
    );
    assert!(text.contains("[Complete]"), "未选项保留，实际 {text:?}");
    assert!(
        !text.contains("Selected:"),
        "预设选项不追加已选行，实际 {text:?}"
    );
    assert!(
        !text.contains("Answered 1 questions"),
        "单问不显示答复问数，实际 {text:?}"
    );
}

/// 自由文本答案（不匹配任何预设选项 label）：选项行之后追加 `Selected:
/// <result>`，且不得给任何选项打 ✓。
#[test]
fn test_ask_user_completed_expanded_free_text_answer() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(super::completed_ask_user_block(
        "5 min recommended",
        &["q1"],
    ));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
    assert!(
        text.contains("Selected: 5 min recommended"),
        "自由文本答案单独一行，实际 {text:?}"
    );
    assert!(
        !text.contains("[\u{2713} Fast]") && !text.contains("[\u{2713} Complete]"),
        "不匹配选项时不得乱标 ✓，实际 {text:?}"
    );
}

/// 多问调用：inline block 只展示首问，选项行之后追加 `Answered N questions`
/// （n = question_ids.len()）。
#[test]
fn test_ask_user_completed_expanded_multi_question_count() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(super::completed_ask_user_block(
        "Fast",
        &["q1", "q2", "q3"],
    ));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = super::all_text(&lines);
    assert!(
        text.contains("Answered 3 questions"),
        "多问调用显示已答复问数，实际 {text:?}"
    );
}

/// Collapsed：单行 `✓ header：answer`，不显示选项与问数；长答案保持单行
/// 截断（不折行）。
#[test]
fn test_ask_user_completed_collapsed_header_and_answer() {
    crate::i18n::init(Some("en"));
    let mut b = super::completed_ask_user_block("Fast", &["q1", "q2"]);
    b.fold = FoldState::Collapsed;
    b.recompute_hash();
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    assert_eq!(lines.len(), 1, "Collapsed 收束为单行");
    let text = super::all_text(&lines);
    assert!(
        text.contains("Length: Fast"),
        "单行含 header 与答案，实际 {text:?}"
    );
    assert!(
        !text.contains("[Fast]") && !text.contains("Answered"),
        "Collapsed 不显示选项与问数，实际 {text:?}"
    );

    // 长答案在窄宽度下仍为单行（截断，不折行）。
    let mut long = super::completed_ask_user_block(
        "5 minutes recommended decision brief with a long explanatory tail",
        &["q1"],
    );
    long.fold = FoldState::Collapsed;
    long.recompute_hash();
    let vm = TuiRenderUnit::TuiAskUserBlock(long);
    let grid = GridSpec::grid_for(30);
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);
    assert_eq!(lines.len(), 1, "窄宽度下 Collapsed 仍单行截断");
    assert!(
        line_text(&lines[0]).contains("Length: 5 minutes"),
        "长答案保留 header 前缀，实际 {:?}",
        line_text(&lines[0])
    );
}

/// Collapsed 无答案（result 空）：单行只显示状态符号 + header。
#[test]
fn test_ask_user_completed_collapsed_without_answer_shows_header() {
    crate::i18n::init(Some("en"));
    let mut b = super::completed_ask_user_block("", &["q1"]);
    b.fold = FoldState::Collapsed;
    b.recompute_hash();
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    assert_eq!(lines.len(), 1, "Collapsed 收束为单行");
    let text = super::all_text(&lines);
    assert!(text.contains("Length"), "header 可见，实际 {text:?}");
    assert!(!text.contains(':'), "无答案时不拼接分隔符，实际 {text:?}");
}

/// header 缺失（防御路径）：expanded 首行回退 i18n 文案，而非答案。
#[test]
fn test_ask_user_completed_expanded_empty_header_falls_back() {
    crate::i18n::init(Some("en"));
    let mut b = super::completed_ask_user_block("Fast", &["q1"]);
    b.question.clear();
    b.recompute_hash();
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) = render::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let first = line_text(&lines[0]);
    assert!(
        first.contains("User answered Peri's questions:"),
        "header 为空时回退 i18n 文案，实际 {first:?}"
    );
    assert!(!first.contains("Fast"), "答案不得作为标题，实际 {first:?}");
}
