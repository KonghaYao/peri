use super::tool_activity_tests::EDIT_DIFF;
use super::*;

// ── [D3 §9] 语义复制：semantic_line_text 变体分派矩阵 ────────────────────

use crate::kit::message_area::render::semantic_line_text;

/// 测试辅助：渲染 VM 后对 `local_idx` 行提取语义文本（复制路径语义——
/// 传入已渲染行，不重渲染 VM）。
pub(super) fn sem_at(vm: &TuiRenderUnit, local_idx: usize, grid: &GridSpec) -> Option<String> {
    let lines = vm_to_lines(vm, grid);
    let line = lines.get(local_idx)?;
    semantic_line_text(vm, local_idx, line, grid)
}

/// 普通 assistant 正文行：剥前缀列（outer + accent + gap），无符号无竖线。
#[test]
fn test_semantic_plain_lines_strip_prefix() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            // 两个 markdown 段落（段落间渲染空行）
            text: "第一行\n\nsecond line with 中文".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    // 行 0 = 前导空行；行 1 = 段落 1；行 2 = 段间空行；行 3 = 段落 2
    let l1 = sem_at(&vm, 1, &grid).expect("段落 1");
    assert_eq!(l1, "第一行", "段落剥前缀，实际: {l1:?}");
    let l2 = sem_at(&vm, 3, &grid).expect("段落 2");
    assert_eq!(l2, "second line with 中文", "段落剥前缀，实际: {l2:?}");
}

/// §6.1/§6.2 无 role label 行：`You` / `Perihelion` 不渲染，正文从第 1 行开始。
#[test]
fn test_no_role_label_line_rendered() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);

    // assistant：行 0 = 前导空行；行 1 = 正文（无 `Perihelion` label）
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "hello".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let text = crate::kit::text_selection::line_to_plain_text(&lines[1]);
    assert!(
        !text.contains("Perihelion"),
        "assistant 无 role label，实际: {text:?}"
    );
    assert_eq!(
        sem_at(&vm, 1, &grid).as_deref(),
        Some("hello"),
        "正文从行 1 开始（前后各 1 空行）"
    );

    // user：行 0 = leading 空行；行 1 = 正文（无 `You` label）
    let user_vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "hello".to_string(),
        reminder: None,
        source: None,
        content_hash: 0,
    });
    let ulines = vm_to_lines(&user_vm, &grid);
    let utext = crate::kit::text_selection::line_to_plain_text(&ulines[1]);
    assert!(
        !utext.contains("You"),
        "user 无 role label，实际: {utext:?}"
    );
    assert_eq!(
        sem_at(&user_vm, 1, &grid).as_deref(),
        Some("hello"),
        "正文从行 1 开始"
    );
}

/// §9.1 Edit/Write 头行只保留 diff 计数（`· +N −M`）——摘要文本含路径，
/// 与 header 的 `input_summary` 重复，不再拼接。
#[test]
fn test_semantic_tool_header_edit_write_count_only() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let card = edit_card_with_diff(EDIT_DIFF);
    let vm = TuiRenderUnit::TuiToolCard(card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Edit src/main.rs \u{b7} +1 \u{b7} -1",
        "header 只含 label+path+计数，实际: {sem:?}"
    );
    assert!(
        !sem.contains("Removed") && !sem.contains("lines changed"),
        "不拼接 output_summary 原文，实际: {sem:?}"
    );

    // Write：新文件摘要（`Wrote 1 line to x.rs`）→ 同样只留计数
    let mut write = tool_card("Write", "/tmp/x.rs", false, false);
    write.output_summary = "Wrote 1 line to /tmp/x.rs".into();
    write.diff = crate::kit::diff_parser::parse_edit_write_summary(
        "Wrote 1 line to /tmp/x.rs",
        Some("/tmp/x.rs"),
    );
    write.recompute_hash();
    let sem = sem_at(&TuiRenderUnit::TuiToolCard(write), 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Write /tmp/x.rs \u{b7} +1",
        "Write 只留计数，实际: {sem:?}"
    );
}

/// tool header 行：`{Verb} {summary}{suffix}`（无符号、无 duration）。
#[test]
fn test_semantic_tool_header() {
    let grid = GridSpec::grid_for(120);
    // Read：label + path + `— N lines` 后缀
    let mut read_card = tool_card("Read", "src/main.rs", false, false);
    read_card.output_summary = "line1\nline2\nline3\n".into();
    read_card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(read_card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Read src/main.rs \u{2014} 3 lines",
        "label+summary+suffix，无符号无时长，实际: {sem:?}"
    );

    // Bash：label + command（不展开时；label 用显示名 Shell）
    let bash_card = tool_card("Bash", "cargo test -p peri-tui", false, false);
    let vm = TuiRenderUnit::TuiToolCard(bash_card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Shell cargo test -p peri-tui",
        "Bash header 复制 command"
    );

    // Bash 展开态：summary 移到 `$ ` 行——header 只留 label
    let mut bash_expanded = tool_card("Bash", "cargo test", false, false);
    bash_expanded.fold = FoldState::Expanded;
    bash_expanded.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(bash_expanded);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(sem, "Shell", "展开态 header 只留 label（显示名）");
}

/// §8 子 agent 工具行语义复制：`{Verb} {summary}`（与主时间线 tool header 同口径，
/// 无符号、无 duration、无缩进/竖线）；原因行 → 纯错误正文（剥缩进）；顶层
/// 单行摘要（非 running）走默认剥离。
#[test]
fn test_semantic_subagent_tool_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Grep", "pattern: x", true, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, true)),
    ]);
    let running = subagent_group(children, true, false, None);
    // 行序：Read（running）/ Bash（completed）/ Grep（error）/ 原因行
    // ——running 有工具时无顶层单行摘要（render_subagent_group_lines 早退）
    let lines = vm_to_lines(&running, &grid);
    assert_eq!(lines.len(), 4, "3 工具行 + 原因行，实际 {}", lines.len());

    let sem_read = sem_at(&running, 0, &grid).expect("Read 工具行");
    assert_eq!(sem_read, "Read src/main.rs", "实际: {sem_read:?}");
    let sem_bash = sem_at(&running, 1, &grid).expect("Bash 工具行");
    assert_eq!(
        sem_bash, "Shell cargo test",
        "Bash → Shell 本地化，实际: {sem_bash:?}"
    );
    let sem_grep = sem_at(&running, 2, &grid).expect("Grep 工具行");
    assert_eq!(sem_grep, "Grep pattern: x", "实际: {sem_grep:?}");
    // 原因行 → 纯错误正文（剥缩进）
    let sem_reason = sem_at(&running, 3, &grid).expect("原因行");
    assert_eq!(
        sem_reason, "Error: something went wrong",
        "实际: {sem_reason:?}"
    );
    // 语义不含 chrome：无 ✓/×/竖线、无前导空格（§11 语义复制要点）
    for s in [&sem_read, &sem_bash, &sem_grep] {
        assert!(
            !s.contains('\u{2713}') && !s.contains('\u{d7}') && !s.contains('\u{2502}'),
            "无符号/竖线 chrome，实际: {s:?}"
        );
        assert!(!s.starts_with(' '), "无前导空格，实际: {s:?}");
    }

    // 非 running（工具行 + 原因行）：工具行语义 `{Verb} {summary}`；
    // 原因行剥缩进。genuine parent error（is_error=true），canonical reason
    // 为空时回退子工具 last_error 兜底。无顶层组头。
    let failed = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
        true,
        None,
    );
    let sem_tool = sem_at(&failed, 0, &grid).expect("工具行");
    assert_eq!(sem_tool, "Grep src", "工具行语义，实际: {sem_tool:?}");
    assert!(
        !sem_tool.contains("Agent explorer"),
        "无顶层组头（agent_name 不出现），实际: {sem_tool:?}"
    );
    let sem_failed_reason = sem_at(&failed, 1, &grid).expect("failed 原因行");
    assert_eq!(
        sem_failed_reason, "Error: something went wrong",
        "非 running 原因行同样剥缩进，实际: {sem_failed_reason:?}"
    );
}

#[test]
fn test_semantic_subagent_without_tools_copies_entry_and_error_reason() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let failed = subagent_group(im::Vector::new(), false, true, Some("child failed"));
    let lines = vm_to_lines(&failed, &grid);
    assert_eq!(lines.len(), 2);
    assert_eq!(sem_at(&failed, 0, &grid).as_deref(), Some("Agent explorer"));
    assert_eq!(sem_at(&failed, 1, &grid).as_deref(), Some("child failed"));
}

/// Bash 展开 `$ cmd` 行保留 command（§9）。
#[test]
fn test_semantic_bash_command_line() {
    let grid = GridSpec::grid_for(120);
    let mut bash_expanded = tool_card("Bash", "cargo test --workspace", false, false);
    bash_expanded.fold = FoldState::Expanded;
    bash_expanded.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(bash_expanded);
    let lines = vm_to_lines(&vm, &grid);
    // 找 `$ cmd` 行（非空且含 `$ `）
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("$ cargo test --workspace"))
        .expect("`$ cmd` 行存在");
    let sem = sem_at(&vm, idx, &grid).expect("$ 行");
    assert_eq!(sem, "$ cargo test --workspace", "保留 $ 前缀与 command");
}

/// diff 行：剥离行号 gutter，保留 `+`/`-` patch 标记（§9）。
#[test]
fn test_semantic_diff_lines_strip_gutter() {
    let grid = GridSpec::grid_for(120);
    let card = edit_card_with_diff(EDIT_DIFF);
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);
    let del_idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 1"))
        .expect("del 行");
    let sem = sem_at(&vm, del_idx, &grid).expect("del 行");
    assert_eq!(
        sem, "-     let x = 1;",
        "patch 标记保留、行号剥离，实际: {sem:?}"
    );
    let add_idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 2"))
        .expect("add 行");
    let sem = sem_at(&vm, add_idx, &grid).expect("add 行");
    assert_eq!(sem, "+     let x = 2;", "add 行同规则，实际: {sem:?}");
    // context 行：纯正文（符号为空格，剥离后无补丁标记）
    let ctx_idx = lines
        .iter()
        .position(|l| line_text(l).contains("fn main() {"))
        .expect("context 行");
    let sem = sem_at(&vm, ctx_idx, &grid).expect("context 行");
    assert_eq!(sem, "fn main() {", "context 行纯正文，实际: {sem:?}");
}

/// 普通输出行以数字开头（如 Bash 输出 `42  foo`）不误判为 diff 行。
#[test]
fn test_semantic_output_line_not_mistaken_for_diff() {
    let grid = GridSpec::grid_for(120);
    let mut card = tool_card("Bash", "echo 42", false, false);
    card.output_summary = "42  foo\nbar".into();
    card.fold = FoldState::Expanded;
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("42  foo"))
        .expect("输出行");
    let sem = sem_at(&vm, idx, &grid).expect("输出行");
    assert_eq!(sem, "42  foo", "输出行不剥内容（非 diff gutter 模式）");
}

/// code block 行：剥 `│ ` gutter 与视觉背景 padding（现状无语言标签行/行号）。
#[test]
fn test_semantic_code_block_strips_gutter() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "```rs\nlet x = 1;\n```".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 1"))
        .expect("code 行");
    let sem = sem_at(&vm, idx, &grid).expect("code 行");
    assert_eq!(sem, "let x = 1;", "剥 `│ ` gutter，实际: {sem:?}");
}

#[test]
fn test_semantic_code_block_preserves_source_trailing_spaces() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "```text\nvalue  \n```".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let idx = lines
        .iter()
        .position(|line| line_text(line).contains("value"))
        .expect("code 行");
    let sem = sem_at(&vm, idx, &grid).expect("code 行");
    assert_eq!(sem, "value  ", "只剥视觉 padding，保留源码尾随空格");
}

/// user bubble：label 行（`You`）与正文行剥离前缀。
#[test]
fn test_semantic_user_bubble() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble {
        text: "重构消息流".to_string(),
        source: None,
        reminder: None,
        content_hash: 0,
    });
    // 行 0 = leading 空行；行 1 = 正文（无 role label 行）
    let body = sem_at(&vm, 1, &grid).expect("正文行");
    assert_eq!(body, "重构消息流", "正文剥前缀，实际: {body:?}");
}
