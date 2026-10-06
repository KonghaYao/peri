use super::*;

// ── Tool activity 行（§6.4）────────────────────────────────────────────

/// 完成工具：`✓ {Verb} {summary}` 单行；summary 用 muted 暗色（§6.4 不抢 label）。
#[test]
fn test_tool_completed_single_line_with_path_color() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "completed 折叠态 = 单行");
    let header = header_of(&lines);
    assert!(header.contains("Read"), "label 应含工具名，实际 {header:?}");
    assert!(header.contains("src/main.rs"), "summary 应含路径");
    let path_span = lines[0]
        .spans
        .iter()
        .find(|s| s.content.as_ref() == " src/main.rs")
        .expect("路径 span");
    assert_eq!(
        path_span.style.fg,
        Some(sem.text.muted),
        "路径 summary 应使用 muted 暗色（不抢 label）"
    );
}

/// Bash 首行 command summary 用 muted 暗色；展开态显示 `$ command` 行（syntax.command）。
#[test]
fn test_tool_bash_command_and_expanded_dollar_line() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;

    let collapsed = TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false));
    let lines = vm_to_lines(&collapsed, &grid);
    let cmd_span = lines[0]
        .spans
        .iter()
        .find(|s| s.content.as_ref() == " cargo test")
        .expect("command span");
    assert_eq!(
        cmd_span.style.fg,
        Some(sem.text.muted),
        "Bash command summary 应使用 muted 暗色"
    );

    let expanded = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "test result: ok".into(),
        ..tool_card("Bash", "cargo test", false, false)
    });
    let lines = vm_to_lines(&expanded, &grid);
    let text = all_text(&lines);
    assert!(text.contains("$ cargo test"), "展开态应有 `$ command` 行");
    assert!(text.contains("\u{2500}"), "展开态应有分隔线");
}

/// 错误态：× + 明确错误词（Failed）；错误输出按 ` - Error: ` 拆行、error 色。
#[test]
fn test_tool_error_splits_and_uses_error_color() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let mut card = tool_card("Edit", "render.rs", true, false);
    card.output_summary = "Tool execution failed: Edit - Error: File not found at /x.rs".into();
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);

    let header = header_of(&lines);
    assert!(header.contains('\u{d7}'), "错误符号 ×");
    assert!(
        header.contains("Failed"),
        "明确错误词 Failed，实际 {header:?}"
    );

    // [§9.2] ` - Error: ` 分隔符拆成两行：首行工具名 + 次行错误详情
    let joined: Vec<String> = lines.iter().map(line_text).collect();
    let joined = joined.join("\n");
    assert!(
        joined.contains("Tool execution failed: Edit"),
        "错误首行含工具名，实际: {joined:?}"
    );
    assert!(
        joined.contains("- Error: File not found at /x.rs"),
        "错误详情独立成行，实际: {joined:?}"
    );

    // 错误输出行 error 色（前缀竖线 tool 角色色除外）
    for line in lines.iter().skip(1) {
        for span in &line.spans {
            if span.content.trim().is_empty() || span.content == "\u{2502}" {
                continue;
            }
            assert_eq!(
                span.style.fg,
                Some(sem.status.error),
                "错误输出行使用 error 色，实际: {:?}",
                span.content
            );
        }
    }
}

/// 折叠的错误工具只保留失败状态行，错误正文等待用户显式展开。
#[test]
fn test_tool_error_collapsed_hides_output() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let mut card = tool_card("Edit", "render.rs", true, false);
    card.fold = FoldState::Collapsed;
    card.output_summary = "Tool execution failed: Edit - Error: File not found at /x.rs".into();
    card.recompute_hash();

    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    assert_eq!(lines.len(), 1, "折叠错误工具不得自动展开正文");
    let header = header_of(&lines);
    assert!(header.contains("Failed"), "失败状态仍须在首行可见");
    assert!(!header.contains("File not found"));
}

/// duration 三档：Wide/Standard 右对齐到屏幕右缘 / Compact+Narrow 隐藏。
#[test]
fn test_tool_duration_three_tiers() {
    let card = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false)); // 400ms → 0.4s

    // Wide（term=120, content=100）：右对齐 → 行末 = 时长，行宽铺满到居中带右缘（line_width）
    let wide = GridSpec::grid_for(120);
    let lines = vm_to_lines(&card, &wide);
    let text = line_text(&lines[0]);
    assert!(
        text.trim_end().ends_with("0.4s"),
        "Wide 时长应右对齐在行尾，实际 {text:?}"
    );
    assert_eq!(
        lines[0].width(),
        wide.line_width() as usize,
        "Wide 行应铺满到居中带右缘（跳过滚动条列）"
    );

    // Standard（term=80, content=74）：同样右对齐（不紧跟 summary）
    let std = GridSpec::grid_for(80);
    let lines = vm_to_lines(&card, &std);
    let text = line_text(&lines[0]);
    assert!(
        text.trim_end().ends_with("0.4s"),
        "Standard 时长应右对齐在行尾，实际 {text:?}"
    );

    // Compact（term=50）与 Narrow（term=30）：隐藏非关键 duration（§11）
    for term in [50u16, 30] {
        let grid = GridSpec::grid_for(term);
        let lines = vm_to_lines(&card, &grid);
        assert!(
            !line_text(&lines[0]).contains("0.4s"),
            "term={term} 应隐藏 duration"
        );
    }
}

/// 时长格式档位（§6.4）：一位小数秒，UI 不出现 `ms`；四舍五入后即 `0.0s`
/// 的（< 50ms）不显示（`None`）；≥1min 回落「Nmin Ms」。
#[test]
fn test_completed_duration_tiers() {
    use super::helpers::format_completed_duration;

    assert_eq!(format_completed_duration(0), None, "0ms 不显示");
    assert_eq!(
        format_completed_duration(37),
        None,
        "37ms 不显示（不是 0.0s）"
    );
    assert_eq!(format_completed_duration(49), None, "49ms 仍不足 0.1s");
    assert_eq!(format_completed_duration(100).as_deref(), Some("0.1s"));
    assert_eq!(format_completed_duration(420).as_deref(), Some("0.4s"));
    assert_eq!(format_completed_duration(1_000).as_deref(), Some("1.0s"));
    assert_eq!(format_completed_duration(12_400).as_deref(), Some("12.4s"));
    assert_eq!(
        format_completed_duration(60_000).as_deref(),
        Some("1min 0s")
    );
    assert_eq!(
        format_completed_duration(125_400).as_deref(),
        Some("2min 5s")
    );
}

/// 亚秒极快（< 50ms）工具行不渲染 duration：行在 summary 处结束，右侧不留
/// 恒定 `0.0s`；同一路径下 ≥0.1s 的时长仍右对齐（对照）。
#[test]
fn test_tool_card_subsecond_duration_omitted() {
    let grid = GridSpec::grid_for(120);

    let mut fast = tool_card("Read", "src/main.rs", false, false);
    fast.completed_duration_ms = Some(37);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(fast), &grid);
    let text = line_text(&lines[0]);
    assert!(
        !text.contains("0.0s") && !text.contains("ms"),
        "实际 {text:?}"
    );
    assert!(
        lines[0].width() < grid.line_width() as usize,
        "无 duration 时不应右对齐补白：宽 {} / line_width {}",
        lines[0].width(),
        grid.line_width()
    );

    // 对照：可见档位仍照常右对齐铺满（fixture 400ms → 0.4s）
    let normal = tool_card("Read", "src/main.rs", false, false);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(normal), &grid);
    assert!(line_text(&lines[0]).trim_end().ends_with("0.4s"));
    assert_eq!(lines[0].width(), grid.line_width() as usize);
}

/// 运行中工具：braille 动画帧 + 活动行（无输出 dump）。
#[test]
fn test_tool_running_symbol_and_no_output() {
    let grid = GridSpec::grid_for(80);
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Bash", "sleep 6", false, true));
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "running 无输出 → 单活动行");
    assert!(
        is_running_frame(&header_of(&lines)),
        "running 符号应为 braille 动画帧（§8.2），实际 {:?}",
        header_of(&lines)
    );
}

/// [Fix F6 §11] Compact/Narrow 断点：tool 展开体最多 2 行（§11「tool summary
/// 最多 2 行」）；Standard 保持 4 行上限（TOOL_OUTPUT_MAX_LINES）。
#[test]
fn test_tool_expanded_output_caps_by_breakpoint() {
    let output = (0..6)
        .map(|i| format!("out line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let expanded = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: output,
        ..tool_card("Bash", "cargo test", false, false)
    });

    // Standard（80 列）：展开体 ≤4 行（TOOL_OUTPUT_MAX_LINES）
    let std_lines = vm_to_lines(&expanded, &GridSpec::grid_for(80));
    let std_body = std_lines
        .iter()
        .filter(|l| line_text(l).contains("out line"))
        .count();
    assert_eq!(std_body, 4, "Standard 展开体上限 4 行");

    // Compact（50 列）与 Narrow（30 列）：展开体 ≤2 行（§11）
    for term in [50u16, 30] {
        let grid = GridSpec::grid_for(term);
        assert!(
            matches!(
                grid.bp,
                crate::kit::message_area::grid::Breakpoint::Compact
                    | crate::kit::message_area::grid::Breakpoint::Narrow
            ),
            "term={term} 应为 Compact/Narrow"
        );
        let lines = vm_to_lines(&expanded, &grid);
        let body = lines
            .iter()
            .filter(|l| line_text(l).contains("out line"))
            .count();
        assert_eq!(body, 2, "term={term} 展开体最多 2 行");
    }
}

// ── System note（§6.6）────────────────────────────────────────────────

#[test]
fn test_system_note_levels() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;

    // Info → divider 线
    let info_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Context compacted \u{273b} 18k \u{2192} 7k".to_string(),
        level: TuiNoteLevel::Info,
        content_hash: 200,
    });
    let info_lines = vm_to_lines(&info_vm, &grid);
    assert_eq!(info_lines.len(), 1, "Info 单行 divider");
    let text = line_text(&info_lines[0]);
    assert!(text.contains("\u{2500}"), "divider 线，实际 {text:?}");
    assert!(text.contains("Context compacted"), "来源文本可见");

    // Warning → `!` + warning accent 首行
    let warn_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Model switched to claude-sonnet-4-5".to_string(),
        level: TuiNoteLevel::Warning,
        content_hash: 201,
    });
    let warn_lines = vm_to_lines(&warn_vm, &grid);
    let header = header_of(&warn_lines);
    assert!(header.contains('!'), "warning 符号 !，实际 {header:?}");
    assert!(
        warn_lines[0].spans[1].style.fg == Some(sem.status.warning),
        "warning accent 色"
    );

    // Error → `×` + error accent 首行；正文 muted
    let err_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Connection lost \u{b7} retrying in 3s".to_string(),
        level: TuiNoteLevel::Error,
        content_hash: 202,
    });
    let err_lines = vm_to_lines(&err_vm, &grid);
    let header = header_of(&err_lines);
    assert!(header.contains('\u{d7}'), "error 符号 ×，实际 {header:?}");
    assert!(header.contains("retrying in 3s"), "恢复动作文本可见");
}

// ── 统一 ToolRenderPlan：presentation 只投影专属内容 ────────────────────

#[test]
fn skill_card_hides_raw_skill_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "skill-1".into(),
        tool_name: "Skill".into(),
        input_summary: "skill: using-superpowers".into(),
        output_summary: "---\nname: using-superpowers\n---\nfull SKILL.md body".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
            name: "using-superpowers".into(),
        }),
        content_hash: 1,
        tool_calls_count: 0,
    };

    let text = all_text(&vm_to_lines(
        &TuiRenderUnit::TuiToolCard(card),
        &GridSpec::grid_for(80),
    ));
    assert!(text.contains("Skill"));
    assert!(text.contains("✓"));
    assert!(text.contains("using-superpowers"));
    assert!(!text.contains("full SKILL.md body"));
    assert!(!text.contains("---"));
}

#[test]
fn todo_card_renders_progress_and_changes_without_raw_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "todo-1".into(),
        tool_name: "TodoWrite".into(),
        input_summary: "todos: 2".into(),
        output_summary: "+[0],[1]".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        fold: FoldState::Expanded,
        user_modified: false,
        presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
            current_items: vec![TuiTodoItem {
                content: "实现语义卡片".into(),
                active_form: None,
                status: TuiTodoStatus::Completed,
            }],
            changes: vec![TuiTodoChange {
                kind: TuiTodoChangeKind::Completed,
                content: "实现语义卡片".into(),
            }],
            is_initial: false,
            completed_count: 1,
            total_count: 1,
        }),
        content_hash: 2,
        tool_calls_count: 0,
    };

    let text = all_text(&vm_to_lines(
        &TuiRenderUnit::TuiToolCard(card),
        &GridSpec::grid_for(80),
    ));
    assert!(text.contains("TodoUpdate"));
    assert!(text.contains("1/1"));
    assert!(text.contains("✓"));
    assert!(text.contains("实现语义卡片"));
    assert!(!text.contains("+[0],[1]"));
}

#[test]
fn todo_card_collapsed_hides_change_details() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "todo-collapsed".into(),
        tool_name: "TodoWrite".into(),
        input_summary: "todos: 1".into(),
        output_summary: "+[0]".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
            current_items: vec![],
            changes: vec![TuiTodoChange {
                kind: TuiTodoChangeKind::Added,
                content: "完成后不自动展开".into(),
            }],
            is_initial: false,
            completed_count: 0,
            total_count: 1,
        }),
        content_hash: 3,
        tool_calls_count: 0,
    };

    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &GridSpec::grid_for(80));
    assert_eq!(lines.len(), 1, "折叠 TodoWrite 只保留状态行");
    assert!(!all_text(&lines).contains("完成后不自动展开"));
}

/// 语义卡在极端窄宽度（Narrow）下不 panic 且不超宽。
#[test]
fn semantic_cards_respect_narrow_widths() {
    crate::i18n::init(Some("en"));
    for width in 1..=8u16 {
        let grid = GridSpec::grid_for(width);
        let card = TuiToolCard {
            tool_id: format!("todo-{width}"),
            tool_name: "TodoWrite".into(),
            input_summary: String::new(),
            output_summary: "+[0]".into(),
            is_error: true,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: Some(37),
            diff: None,
            fold: FoldState::Collapsed,
            user_modified: false,
            presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
                current_items: vec![],
                changes: vec![TuiTodoChange {
                    kind: TuiTodoChangeKind::Added,
                    content: "这是一个足够长的任务标题，用于验证窄终端截断行为".into(),
                }],
                is_initial: true,
                completed_count: 0,
                total_count: 1,
            }),
            content_hash: width as u64,
            tool_calls_count: 0,
        };
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        // 截断省略号允许 +1 列（content 列内放不下的省略号落在 gap 前）
        let max_width = grid.first_prefix_width() + grid.content_width() + 1;
        for line in &lines {
            assert!(
                line.width() <= max_width,
                "term={width}: 行宽 {} 超过 {max_width}",
                line.width()
            );
        }
    }
}

// ── 工具头行后缀（历史行为保留）────────────────────────────────────────

/// Glob/Grep 完成后头行显示 `— N matches`；错误态不显示。
#[test]
fn test_glob_grep_header_match_suffix() {
    crate::i18n::init(Some("zh-CN"));
    let grid = GridSpec::grid_for(120);
    let files: Vec<String> = (0..163)
        .map(|i| format!("/repo/peri-tui/src/kit/file_{i}.rs"))
        .collect();
    let output = files.join("\n");

    for tool_name in ["Glob", "Grep"] {
        let card = TuiToolCard {
            output_summary: output.clone(),
            ..tool_card(
                tool_name,
                r#"pattern: "peri-tui/src/**/*.rs""#,
                false,
                false,
            )
        };
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        let header = header_of(&lines);
        assert!(
            header.contains("— 163 matches"),
            "{tool_name} 头行应包含 '— 163 matches'，实际: {header:?}"
        );
        assert!(
            header.contains("pattern:"),
            "{tool_name} 头行应包含 pattern 参数，实际: {header:?}"
        );
    }

    // 错误态：头行不得包含匹配数后缀
    for tool_name in ["Glob", "Grep"] {
        let card = tool_card(tool_name, "pattern: x", true, false);
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        let header = header_of(&lines);
        assert!(
            !header.contains("matches"),
            "{tool_name} 错误态头行不应包含 'matches'，实际: {header:?}"
        );
    }
}

/// §6.7 completed 子 agent：组只渲染嵌套工具行（无单行组头/计数 meta），
/// 工具行整体不越过消息区右缘（Wide 右对齐 duration 不被 fit 截断）。
#[test]
fn test_subagent_completed_shows_tool_lines_only() {
    // subagent：completed + 1 个 Bash 工具 → 只渲染工具行
    let group = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        instance_id: "instance-a1".into(),
        agent_id: "a1".into(),
        agent_name: "general-purpose".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Bash",
            "echo hello-subagent-internal",
            false,
            false,
        ))]),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    let wide = GridSpec::grid_for(120);
    let lines = vm_to_lines(&group, &wide);
    assert_eq!(
        lines.len(),
        1,
        "completed + 1 工具 → 1 个工具行，实际 {}",
        lines.len()
    );
    let text = line_text(&lines[0]);
    assert!(
        text.contains("echo hello-subagent-internal") && text.contains("Shell"),
        "工具行显示 Bash → Shell 与摘要，实际 {text:?}"
    );
    assert!(
        !text.contains("general-purpose"),
        "不再渲染单行组头（agent_name），实际 {text:?}"
    );
    // 行宽 = 居中带右缘（line_width，右对齐铺满）
    assert_eq!(lines[0].width(), wide.line_width() as usize);
}

// ── [G-Diff] §6.5 diff 展开体渲染（120/80/48 列 golden）──────────────────

/// 构造带 diff 的 Edit 卡片（fold=Expanded 展示展开体）。
/// output_summary 设为 diff 文本本身（真实形态——Edit 输出即 diff 文本）。

pub(super) const EDIT_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,6 +10,7 @@ pub fn main() {
 fn main() {
-    let x = 1;
+    let x = 2;
     println!(\"{}\", x);
 }
";

/// 120 列 Wide：header `path +N −M` + hunk 头 + 行号 gutter + patch 标记。
#[test]
fn test_diff_block_renders_wide_120() {
    crate::i18n::init(Some("en"));
    let card = edit_card_with_diff(EDIT_DIFF);
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);

    assert!(text.contains("src/main.rs"), "header 含路径，实际: {text}");
    assert!(
        text.contains("+1"),
        "header 含 +N 计数（1 add），实际: {text}"
    );
    assert!(
        text.contains("\u{2212}1"),
        "header 含 −M 计数（1 del，U+2212），实际: {text}"
    );
    assert!(
        text.contains("@@ -10,6 +10,7 @@"),
        "hunk 头渲染，实际: {text}"
    );
    assert!(text.contains("10"), "context 行号 gutter");
    assert!(text.contains("11 -"), "del 行保留 `-` patch 标记");
    assert!(text.contains("11 +"), "add 行保留 `+` patch 标记");
    assert!(text.contains("fn main() {"), "context 正文");
    assert!(!text.contains("diff --git"), "diff 元数据头不进入渲染");
    assert!(!text.contains("index 1234567"), "index 头不进入渲染");

    // 原始输出行被 diff 块替代（Edit 输出即 diff 文本，避免重复）
    assert!(!text.contains("done"), "diff 卡不显示原始 output_summary");
}

/// 80 列 Standard：行号 gutter 保留，代码不软换行（硬截断）。
#[test]
fn test_diff_block_renders_standard_80() {
    crate::i18n::init(Some("en"));
    let long_diff = "\
--- a/long.rs
+++ b/long.rs
@@ -1,1 +1,1 @@
-very long deleted line that definitely exceeds the eighty column content budget in this terminal width
+replacement line that is also extremely long and will be truncated by width not wrapped
";
    let card = edit_card_with_diff(long_diff);
    let grid = GridSpec::grid_for(80);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    for line in &lines {
        let w = line_text(line).width();
        assert!(
            w <= grid.line_width() as usize,
            "diff 行不软换行（硬截断），宽度 {w} > {}，行: {:?}",
            grid.line_width(),
            line_text(line)
        );
    }
    let text = all_text(&lines);
    assert!(text.contains("+ replacement"), "patch 标记保留");
    assert!(text.contains("- very long"), "patch 标记保留");
}

/// 48 列 Compact：先隐藏行号 gutter，再裁切代码（§6.5「窄屏先隐行号」）。
#[test]
fn test_diff_block_hides_gutter_at_48() {
    crate::i18n::init(Some("en"));
    let card = edit_card_with_diff(EDIT_DIFF);
    let grid = GridSpec::grid_for(48);
    assert!(matches!(grid.bp, Breakpoint::Compact), "48 列是 Compact");
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);

    assert!(text.contains("fn main() {"), "正文仍可见");
    assert!(text.contains("+"), "patch 标记仍可见");
    assert!(text.contains("-"), "patch 标记仍可见");
    assert!(
        text.contains("@@ -10,6 +10,7 @@"),
        "hunk 头（dim 元信息）窄屏仍显示"
    );

    // 行号 gutter 隐藏：`11 -` 的 `11 ` 不应出现在行首（符号后正文紧跟）
    let del_line = lines
        .iter()
        .find(|l| line_text(l).contains("let x = 1"))
        .map(line_text)
        .unwrap_or_default();
    assert!(
        !del_line.trim_start().starts_with("11"),
        "窄屏无行号列，实际: {del_line:?}"
    );
    assert!(
        del_line.contains("let x = 1"),
        "patch 标记 + 正文，实际: {del_line:?}"
    );
}

/// 截断指示：>8 change 行 → `… +N more lines`（§6.5）。
#[test]
fn test_diff_block_more_lines_indicator() {
    crate::i18n::init(Some("en"));
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,20 +1,20 @@\n");
    for i in 0..10 {
        text.push_str(&format!("- old {i}\n"));
    }
    for i in 0..10 {
        text.push_str(&format!("+ new {i}\n"));
    }
    let card = edit_card_with_diff(&text);
    assert!(card.diff.is_some(), "截断 diff 仍可解析（不降级）");
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);
    assert!(
        text.contains("\u{2026} +12 more lines"),
        "截断指示（12 剩余 change 行），实际: {text}"
    );
}

/// 解析失败（非 diff 输出）→ diff=None → 渲染保持历史行为（无 diff 块）。
#[test]
fn test_diff_block_falls_back_when_unparsable() {
    crate::i18n::init(Some("en"));
    let mut card = tool_card("Edit", "src/x.rs", false, false);
    card.fold = FoldState::Expanded;
    card.output_summary = "Wrote 3 lines to x.rs".into();
    card.recompute_hash();
    assert!(card.diff.is_none(), "非 diff 输出静默降级");
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);
    assert!(text.contains("Wrote 3 lines to x.rs"), "兜底显示原始输出");
}

// ── wave 3（workspace）：effective name 与裸名渲染逐位一致 ────────────────────

/// workspace Bash（effective name）展开态仍显示 `$ command` 行 + 分隔线，
/// 且渲染文本与裸名卡片**逐位一致**。
#[test]
fn workspace_bash_card_keeps_dollar_prefix() {
    let grid = GridSpec::grid_for(80);
    let render = |tool_name: &str| {
        let card = TuiToolCard {
            fold: FoldState::Expanded,
            output_summary: "test result: ok".into(),
            ..tool_card(tool_name, "cargo test", false, false)
        };
        all_text(&vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid))
    };

    let text = render("mcp__workspace__Bash");
    assert!(
        text.contains("$ cargo test"),
        "workspace Bash 展开态应有 `$ command` 行，实际: {text:?}"
    );
    assert!(
        text.contains("\u{2500}"),
        "workspace Bash 展开态应有分隔线，实际: {text:?}"
    );
    assert_eq!(
        text,
        render("Bash"),
        "effective name 与裸名的渲染文本必须逐位一致"
    );
}

/// workspace Read / Edit（effective name）完成态头行后缀与裸名一致：
/// Read `— N lines`、Edit `· +N · -M`。
#[test]
fn workspace_read_and_edit_cards_keep_header_suffix() {
    let grid = GridSpec::grid_for(120);

    for tool_name in ["mcp__workspace__Read", "Read"] {
        let card = TuiToolCard {
            output_summary: "first\nsecond\nthird".into(),
            ..tool_card(tool_name, "src/main.rs", false, false)
        };
        let header = header_of(&vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid));
        assert!(
            header.contains("\u{2014} 3 lines"),
            "{tool_name} 完成态头行应包含 '— 3 lines'，实际: {header:?}"
        );
    }

    for tool_name in ["mcp__workspace__Edit", "Edit"] {
        let card = TuiToolCard {
            output_summary: "Added 3 lines to render.rs".into(),
            diff: Some(TuiDiffBlock {
                path: "render.rs".into(),
                hunks: Vec::new(),
                is_binary: false,
                is_too_large: false,
                is_new_file: false,
                more_change_lines: 0,
                adds: 3,
                dels: 1,
            }),
            ..tool_card(tool_name, "render.rs", false, false)
        };
        let header = header_of(&vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid));
        assert!(
            header.contains("\u{b7} +3 \u{b7} -1"),
            "{tool_name} 完成态头行应包含 '· +3 · -1'，实际: {header:?}"
        );
    }
}
