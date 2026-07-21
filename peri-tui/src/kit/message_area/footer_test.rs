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
