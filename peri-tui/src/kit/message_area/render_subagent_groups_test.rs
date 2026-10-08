use super::*;

// ── SubAgent 单行（§6.7）───────────────────────────────────────────────

/// §6.7 running 子 agent：显示最近 ≤3 个子工具调用行（最新在前）。
/// 工具行为嵌套从属弱化形态（设计文档 §3）：续行竖线 + 2 格缩进 + 无 bold label
/// + dim 符号——与主时间轴 tool activity row（bold primary + 状态色）差异化。
#[test]
fn test_subagent_running_shows_recent_tool_lines() {
    let grid = GridSpec::grid_for(80);
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Read", "file-a.rs", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Glob", "src/**/*.rs", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false)),
        // 最新子工具仍在运行
        TuiRenderUnit::TuiToolCard(tool_card("Read", "file-d.rs", false, true)),
    ]);
    let running = subagent_group(children, true, false, None);
    let lines = vm_to_lines(&running, &grid);

    // 最多 3 行（SUBAGENT_TOOL_LINES），反向取最近工具，最新在前
    assert_eq!(
        lines.len(),
        3,
        "最多显示最近 3 个工具行，实际 {}",
        lines.len()
    );
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    assert!(
        texts[0].contains("file-d.rs") && texts[0].contains("Read"),
        "最新工具在最前，实际 {texts:?}"
    );
    assert!(
        texts[1].contains("cargo test") && texts[1].contains("Shell"),
        "次新工具（Bash → Shell），实际 {texts:?}"
    );
    assert!(texts[2].contains("Glob"), "第三新工具，实际 {texts:?}");
    assert!(
        !texts[0].contains("file-a.rs"),
        "超过 3 个的旧工具不显示，实际 {texts:?}"
    );

    // running 工具行用动画符号（braille 帧 / ASCII *）
    assert!(
        is_running_frame(&header_of(&lines)),
        "running 工具行用动画符号，实际 {:?}",
        texts[0]
    );

    // 首列结构：`[outer 空][│][gap][2 格缩进]`（设计文档 §11 层级结构）——
    // 工具行永远不是独立 entry 首行（不再用 first_prefix），正文起点 = content + 2
    let sem = THEME_ATOM.state().read().semantic;
    let s = &lines[0].spans;
    assert_eq!(s[0].content, " ", "outer 空列");
    assert_eq!(s[1].content, "\u{2502}", "续行竖线");
    assert_eq!(s[2].content, "  ", "gap（80 列 = 2）");
    assert_eq!(s[3].content, "  ", "2 格缩进 SUBAGENT_TOOL_INDENT");
    // 符号 span（符号 + 分隔空格）dim 色（P2 低显著，对照主时间线 status.running）
    assert_eq!(
        s[4].style.fg,
        Some(sem.text.dim),
        "running 符号 fg = text.dim，实际 {:?}",
        s[4].style
    );
    // Verb span 无 BOLD（P2 权重弱化——bold 是主时间线工具的专属锚点）
    let verb = lines[0]
        .spans
        .iter()
        .find(|sp| sp.content == "Read")
        .expect("Verb span");
    assert!(
        !verb.style.add_modifier.contains(Modifier::BOLD),
        "label 无 bold，实际 {:?}",
        verb.style
    );

    // completed 工具行 duration 右对齐在行尾
    assert!(
        texts[1].trim_end().ends_with("0.4s"),
        "completed 工具行 duration 在行尾，实际 {:?}",
        texts[1]
    );
}

/// §6.7 running 子 agent 但组内无任何工具调用：不渲染组头，整组留空。
#[test]
fn test_subagent_running_no_tools_renders_empty() {
    let grid = GridSpec::grid_for(80);
    let running = subagent_group(im::Vector::new(), true, false, None);
    let lines = vm_to_lines(&running, &grid);
    assert!(
        lines.is_empty(),
        "无工具 → 整组留空（不渲染组头），实际 {:?}",
        all_text(&lines)
    );
}

/// Narrow 断点：符号位省略（设计文档 §6 断点表）——`[outer][│][gap=1][2 格缩进]`
/// 后直接是 Verb，无 braille/✓/× 字符；错误信号由错误词与原因行兜底。
#[test]
fn test_subagent_tool_line_narrow_omits_symbol() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(30); // Narrow: gap=1
    let running = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Read",
            "src/main.rs",
            false,
            true,
        ))]),
        true,
        false,
        None,
    );
    let lines = vm_to_lines(&running, &grid);
    assert_eq!(lines.len(), 1, "1 个工具行，实际 {}", lines.len());
    let s = &lines[0].spans;
    assert_eq!(s[0].content, " ", "outer 空列");
    assert_eq!(s[1].content, "\u{2502}", "续行竖线");
    assert_eq!(s[2].content, " ", "Narrow gap=1");
    assert_eq!(s[3].content, "  ", "2 格缩进 SUBAGENT_TOOL_INDENT");
    let text = line_text(&lines[0]);
    assert!(
        !is_running_frame(&text) && !text.contains('✓') && !text.contains('×'),
        "Narrow 无符号位，实际 {text:?}"
    );
    assert!(
        text.contains("Read") && text.contains("src/main.rs"),
        "Verb + summary 保留，实际 {text:?}"
    );
    // 无 duration（§11 Compact/Narrow 隐藏非关键 duration——place_meta 既有行为）
    assert!(
        !text.trim_end().chars().any(|c| c.is_ascii_digit()),
        "Narrow 无 duration，实际 {text:?}"
    );
}

/// §6.7 running 子 agent 已有失败工具：工具行 + 原因行。
/// error 工具行符号升级 status.error + ` — Failed` 错误词（P3 错误不弱化）；
/// 原因行缩进与工具行同列对齐（设计文档 §5）。
#[test]
fn test_subagent_running_with_failed_tool_shows_reason() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Grep", "src", true, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Read", "a.rs", false, true)),
    ]);
    let running = subagent_group(children, true, false, None);
    let lines = vm_to_lines(&running, &grid);
    // 行序：最近工具行（Read running）→ error 工具行（Grep）→ 原因行
    assert_eq!(lines.len(), 3, "工具行 ×2 + 原因行，实际 {}", lines.len());
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    assert!(
        texts[2].contains("Error: something went wrong"),
        "失败原因行可见，实际 {texts:?}"
    );

    // error 工具行：× 符号 + ` — Failed` 错误词
    let err_text = line_text(&lines[1]);
    assert!(
        err_text.contains('\u{d7}'),
        "失败工具行用 × 符号，实际 {err_text:?}"
    );
    assert!(
        err_text.contains(" \u{2014} Failed"),
        "error 行含 ` — Failed` 错误词，实际 {err_text:?}"
    );
    // error 符号 span fg = status.error（P3 升级，对照 running/success 的 text.dim）
    let sym_span = lines[1]
        .spans
        .iter()
        .find(|sp| sp.content.contains('\u{d7}'))
        .expect("error 符号 span");
    assert_eq!(
        sym_span.style.fg,
        Some(sem.status.error),
        "error 符号 fg = status.error，实际 {:?}",
        sym_span.style
    );
    // 错误词 span：bold + error 色（§6.4 主时间线同款）
    let failed_span = lines[1]
        .spans
        .iter()
        .find(|sp| sp.content.contains("Failed"))
        .expect("错误词 span");
    assert_eq!(
        failed_span.style.fg,
        Some(sem.status.error),
        "错误词 fg = status.error，实际 {:?}",
        failed_span.style
    );
    assert!(
        failed_span.style.add_modifier.contains(Modifier::BOLD),
        "错误词 bold，实际 {:?}",
        failed_span.style
    );
    // 原因行与工具行同列对齐（`[outer 空][│][gap][2 格缩进]` 前缀）
    assert!(
        texts[2].starts_with(" \u{2502}    "),
        "原因行缩进与工具行对齐，实际 {:?}",
        texts[2]
    );
}

/// subagent block 终态只由 canonical `is_error` 决定：
/// - completed parent + 失败 child tool → ✓ 单行摘要，无 parent 原因行
///   （bug 回归：child tool error 不再提升 block error；child 卡自身 error
///   展示不变，nested detail 可见）；
/// - genuine parent error（is_error=true）→ × + 原因行，canonical reason
///   （error_reason）优先，空时回退 child last_error 兜底；
/// - completed 无失败 → ✓ + 结果摘要（历史断言保留）。
#[test]
fn test_subagent_failed_reason_line_and_completed() {
    let grid = GridSpec::grid_for(80);

    // completed parent（is_error=false）+ 失败 child tool → 工具行（×），
    // 无原因行（completed 成功组不显示 nested error 原因，§6.7 语义）
    let completed_with_failed_child = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
        false,
        None,
    );
    let lines = vm_to_lines(&completed_with_failed_child, &grid);
    assert_eq!(
        lines.len(),
        1,
        "completed + 1 工具 → 1 个工具行，实际 {:?}",
        all_text(&lines)
    );
    assert!(
        header_of(&lines).contains('\u{d7}'),
        "child tool error 符号 ×，实际 {:?}",
        header_of(&lines)
    );

    // genuine parent error + canonical reason → × + 原因行（canonical 优先，
    // 不显示 child tool 的 last_error）
    let failed = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
        true,
        Some("loop failed: llm error"),
    );
    let lines = vm_to_lines(&failed, &grid);
    let text = all_text(&lines);
    assert!(header_of(&lines).contains('\u{d7}'), "failed 符号 ×");
    assert!(
        text.contains("loop failed: llm error"),
        "canonical 原因行可见，实际 {text:?}"
    );
    // 原因行（第 2 行）必须是 canonical reason，而非 child tool 的 last_error
    assert!(
        text.lines()
            .nth(1)
            .unwrap_or("")
            .contains("loop failed: llm error"),
        "原因行优先 canonical reason，实际 {text:?}"
    );
    // 原因行（非 running 分支同样走 cont_prefix + 2 格缩进，与工具行同列）
    assert!(
        text.lines()
            .nth(1)
            .unwrap_or("")
            .starts_with(" \u{2502}    "),
        "failed 原因行缩进对齐，实际 {text:?}"
    );

    // genuine parent error 但 canonical reason 为空（error_reason=None）：
    // 回退子工具 last_error 兜底（非空、可读，不渲染空行）
    let fallback = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
        true,
        None,
    );
    let lines = vm_to_lines(&fallback, &grid);
    let text = all_text(&lines);
    assert!(header_of(&lines).contains('\u{d7}'), "failed 符号 ×");
    assert!(
        text.contains("Error: something went wrong"),
        "空 canonical reason 回退 child last_error，实际 {text:?}"
    );

    // genuine parent error 且无工具、无原因 → 整组留空（不渲染组头）
    let no_tool_error = subagent_group(im::Vector::new(), false, true, None);
    let lines = vm_to_lines(&no_tool_error, &grid);
    assert!(
        lines.is_empty(),
        "无工具 error 且无原因 → 整组留空，实际 {:?}",
        all_text(&lines)
    );

    // completed parent 无失败 → 只渲染工具行（无组头/结果摘要）
    let completed = subagent_group(
        im::Vector::from(vec![
            TuiRenderUnit::TuiToolCard(tool_card("Read", "a.rs", false, false)),
            TuiRenderUnit::TuiAssistantBubble(
                TuiAssistantBubble {
                    // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                    started_at: None,
                    duration_ms: None,
                    text: "Found 8 UI patterns".into(),
                    reasoning: None,
                    message_id: None,
                    content_hash: 7,
                }
                .into(),
            ),
        ]),
        false,
        false,
        None,
    );
    let lines = vm_to_lines(&completed, &grid);
    assert_eq!(
        lines.len(),
        1,
        "completed + 1 工具 → 1 个工具行，实际 {}",
        lines.len()
    );
    let text = all_text(&lines);
    assert!(
        text.contains("Read") && text.contains("a.rs"),
        "工具行可见，实际 {text:?}"
    );
    assert!(
        !text.contains("Found 8 UI patterns"),
        "不渲染结果摘要（组头已取消），实际 {text:?}"
    );
}

// ── 分组 / divider / todo（§6.6/§6.7/§6.9/§7）───────────────────────────

/// TuiCollapsedGroup：`▸ {title}`（title 含隐藏数；组后相邻 error → `· N failed`）。
#[test]
fn test_collapsed_group_line() {
    let grid = GridSpec::grid_for(80);
    let mut group = TuiCollapsedGroup {
        title: "Read 3 · Glob 2".into(),
        count: 5,
        failed_count: 0,
        view_models: vec![],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group.recompute_hash();
    let lines = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid);
    assert_eq!(lines.len(), 1);
    let text = line_text(&lines[0]);
    assert!(text.contains("\u{25b8}"), "折叠符号 ▸，实际 {text:?}");
    assert!(text.contains("Read 3 · Glob 2"), "标题含隐藏数");
    assert!(
        !text.contains("failed"),
        "无相邻 error 时无失败后缀，实际 {text:?}"
    );
}

/// [D2] 组后相邻 error 数 >0 → 标题追加 `· N failed`（error 色 span），
#[test]
fn test_expanded_group_renders_member_tools() {
    let grid = GridSpec::grid_for(80);
    let mut group = TuiCollapsedGroup {
        title: "Read 1".into(),
        count: 1,
        failed_count: 0,
        view_models: vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Read", "a.rs", false, false,
        ))],
        fold: FoldState::Expanded,
        content_hash: 0,
    };
    group.recompute_hash();

    let text = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("a.rs"), "展开后应渲染组内工具，实际 {text:?}");
}

#[test]
fn test_collapsed_group_narrow_hides_whole_hint_within_grid_width() {
    let grid = GridSpec::grid_for(24);
    let mut group = TuiCollapsedGroup {
        title: "读取工具 100 · Glob 100".into(),
        count: 200,
        failed_count: 3,
        view_models: vec![],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group.recompute_hash();

    let lines = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid);
    let text = line_text(&lines[0]);
    assert!(
        text.contains("· 3 failed"),
        "失败后缀必须保留，实际 {text:?}"
    );
    assert!(
        !text.contains("click") && !text.contains("Enter"),
        "hint 放不下时必须整体隐藏，实际 {text:?}"
    );
    assert!(
        text.width() <= grid.line_width() as usize,
        "display width 不得超过 grid：{} > {}，实际 {text:?}",
        text.width(),
        grid.line_width()
    );
}

/// 与 `+N −M` 计数（diff_change_summary）不混淆。
#[test]
fn test_collapsed_group_line_with_failed_count() {
    let grid = GridSpec::grid_for(80);
    let mut group = TuiCollapsedGroup {
        title: "Read 2".into(),
        count: 2,
        failed_count: 1,
        view_models: vec![],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group.recompute_hash();
    let lines = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid);
    assert_eq!(lines.len(), 1);
    let text = line_text(&lines[0]);
    assert!(text.contains("Read 2"), "标题含隐藏数");
    assert!(text.contains("· 1 failed"), "失败后缀，实际 {text:?}");
    // 失败后缀使用 status.error 色（只染后缀，不染整行）
    let line = &lines[0];
    let failed = line
        .spans
        .iter()
        .find(|span| span.content.contains("failed"))
        .expect("失败后缀是独立 span");
    assert_ne!(failed.style.fg, None, "失败后缀必须有 error 前景色");

    // 窄屏：title 截断优先于失败后缀（失败数不可被截断吞掉）
    let narrow = GridSpec::grid_for(40);
    let mut group2 = TuiCollapsedGroup {
        title: "Read 100 · Glob 100 · Bash 100".into(),
        count: 300,
        failed_count: 3,
        view_models: vec![],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group2.recompute_hash();
    let lines2 = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group2), &narrow);
    let text2 = line_text(&lines2[0]);
    assert!(
        text2.contains("· 3 failed"),
        "窄屏失败数仍可见，实际 {text2:?}"
    );
}

/// Divider：无 label 纯分隔线填满 content 列；有 label 显示 `── label ──`。
#[test]
fn test_divider_lines() {
    let grid = GridSpec::grid_for(80);
    let plain = vm_to_lines(
        &TuiRenderUnit::TuiDivider(TuiDivider {
            label: None,
            content_hash: 0,
        }),
        &grid,
    );
    let text = line_text(&plain[0]);
    assert!(text.contains("\u{2500}"), "divider 线");
    assert!(text.trim().chars().all(|c| c == '\u{2500}' || c == ' '));

    let labeled = vm_to_lines(
        &TuiRenderUnit::TuiDivider(TuiDivider {
            label: Some("Round 3".into()),
            content_hash: 1,
        }),
        &grid,
    );
    let text = line_text(&labeled[0]);
    assert!(text.contains("Round 3"), "label 可见");
}

/// TuiTodoSummary：`◼ {3/7 tasks · Running tests}`。
#[test]
fn test_todo_summary_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let vm = TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("3/7 tasks · Running tests".into()));
    let lines = vm_to_lines(&vm, &grid);
    let text = line_text(&lines[0]);
    assert!(text.contains("\u{25fc}"), "todo 符号 ◼");
    assert!(text.contains("3/7 tasks · Running tests"));
}
