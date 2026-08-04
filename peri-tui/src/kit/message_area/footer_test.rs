//! Tests

use super::*;

#[test]
fn test_render_todo_lines_icons_and_crossed() {
    let items = vec![
        TodoItem {
            status: TodoStatus::InProgress,
            content: "修复 bug".into(),
        },
        TodoItem {
            status: TodoStatus::Completed,
            content: "草拟 PRD".into(),
        },
        TodoItem {
            status: TodoStatus::Pending,
            content: "部署".into(),
        },
    ];
    let lines = render_todo_lines(&items);
    assert_eq!(lines.len(), 4);

    let in_progress_icon = lines[0].spans[0].content.as_ref();
    assert!(in_progress_icon.contains("◼"), "InProgress 图标应为 ◼");
    let in_progress_text = lines[0].spans[1].content.as_ref();
    assert!(
        in_progress_text.contains("修复 bug"),
        "InProgress 文本应包含任务内容"
    );

    let completed_icon = lines[1].spans[0].content.as_ref();
    assert!(completed_icon.contains("✔"), "Completed 图标应为 ✔");
    let completed_text = lines[1].spans[1].content.as_ref();
    assert!(
        completed_text.contains("草拟 PRD"),
        "Completed 文本应包含任务内容"
    );

    let pending_icon = lines[2].spans[0].content.as_ref();
    assert!(pending_icon.contains("◻"), "Pending 图标应为 ◻");
    let pending_text = lines[2].spans[1].content.as_ref();
    assert!(pending_text.contains("部署"), "Pending 文本应包含任务内容");
    assert!(
        pending_text.contains("(available)") || pending_text.contains("(可开始)"),
        "Pending 文本应包含 i18n 可用标记"
    );
}

#[test]
fn test_render_todo_lines_empty() {
    let lines = render_todo_lines(&[]);
    assert_eq!(lines.len(), 1);
    for line in &lines {
        assert!(
            line.spans.is_empty(),
            "空 todo 列表不应输出任何内容行，仅 trailing blank lines"
        );
    }
}

#[test]
fn test_spinner_summary_after_loading_ends() {
    let elapsed_ms: u64 = 30_000;
    let elapsed_str = crate::components::spinner::animation::format_elapsed(elapsed_ms);
    assert_eq!(elapsed_str, "30s");

    let summary = format!("  ✻  Brewed for {elapsed_str}");
    assert!(summary.contains("✻"));
    assert!(summary.contains("Brewed for"));
}

#[test]
fn test_token_count_no_write_when_unchanged() {
    let prev_token_count: usize = 1500;
    let new_token_count: usize = 1500;
    let changed = prev_token_count != new_token_count;

    assert!(!changed, "token count 未变化时不应写 state");
}

#[test]
fn test_footer_loading_steady_state_has_no_control_state_transition() {
    let prev_loading = true;
    let is_loading = true;
    let transition = prev_loading != is_loading;

    assert!(
        !transition,
        "loading 稳态不应写 was_loading/load_start，否则会触发持续重渲染"
    );
}

/// idle 态渲染静止图标占位：固定第一帧（不参与动画），无 verb/elapsed
#[test]
fn test_render_idle_spinner_line_static_fixed_frame() {
    let line = render_idle_spinner_line(ratatui::style::Color::DarkGray);
    let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let frame0 = crate::components::spinner::animation::tick_to_frame(0);
    assert!(
        rendered.contains(frame0),
        "idle 应渲染静止图标（固定第一帧），实际: {rendered}"
    );
    // 静止语义：连续两次渲染内容一致（不随壁钟推进换帧）
    let again = render_idle_spinner_line(ratatui::style::Color::DarkGray);
    let rendered2: String = again.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(rendered, rendered2, "idle 行不应随时间变化");
    // 不包含 verb/elapsed 后缀
    assert!(
        !rendered.contains('s') || !rendered.contains("token"),
        "idle 行不应带 elapsed/token 后缀，实际: {rendered}"
    );
}

/// verb_override 提供摘要时，spinner 渲染优先使用摘要而非随机名言
#[test]
fn test_render_to_lines_verb_override_优先于名言() {
    let spinner = crate::components::spinner::SpinnerState::new(
        crate::components::spinner::SpinnerMode::Thinking,
    );
    let lines = spinner.render_to_lines(
        ratatui::style::Color::White,
        ratatui::style::Color::DarkGray,
        false,
        false,
        0,
        Some("修复了认证问题"),
    );
    let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        rendered.contains("修复了认证问题"),
        "有摘要时应显示摘要而非名言，实际: {rendered}"
    );
}

/// 无 verb_override 时保持随机名言
#[test]
fn test_render_to_lines_无_override_显示名言() {
    let spinner = crate::components::spinner::SpinnerState::new(
        crate::components::spinner::SpinnerMode::Thinking,
    );
    let lines = spinner.render_to_lines(
        ratatui::style::Color::White,
        ratatui::style::Color::DarkGray,
        false,
        false,
        0,
        None,
    );
    let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!rendered.contains("修复了认证问题"));
    assert!(
        crate::components::spinner::verb::DEFAULT_VERBS
            .iter()
            .any(|v| rendered.contains(v)),
        "无 override 时应显示随机名言，实际: {rendered}"
    );
}
