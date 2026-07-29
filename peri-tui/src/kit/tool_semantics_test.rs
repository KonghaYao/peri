use super::{TodoSnapshot, presentation_for};
use crate::kit::tui_render_unit::{TuiTodoChangeKind, TuiTodoStatus, TuiToolPresentation};
use serde_json::json;

#[test]
fn skill_presentation_uses_skill_and_legacy_skill_name() {
    assert_eq!(
        presentation_for("Skill", &json!({"skill": "using-superpowers"}), None),
        TuiToolPresentation::Skill(crate::kit::tui_render_unit::TuiSkillPresentation {
            name: "using-superpowers".into(),
        }),
    );
    assert_eq!(
        presentation_for("SkillTool", &json!({"skill_name": "legacy-skill"}), None),
        TuiToolPresentation::Skill(crate::kit::tui_render_unit::TuiSkillPresentation {
            name: "legacy-skill".into(),
        }),
    );
}

#[test]
fn malformed_todo_input_falls_back_to_generic_card() {
    assert_eq!(
        presentation_for("TodoWrite", &json!({"todos": "not-an-array"}), None),
        TuiToolPresentation::Generic,
    );
}

#[test]
fn skill_without_a_parseable_name_stays_safe_and_semantic() {
    assert_eq!(
        presentation_for("Skill", &json!({"skill": 42}), None),
        TuiToolPresentation::Skill(crate::kit::tui_render_unit::TuiSkillPresentation {
            name: "unknown".into(),
        }),
    );
}

#[test]
fn initial_todo_snapshot_reports_progress_and_added_items() {
    let presentation = presentation_for(
        "TodoWrite",
        &json!({
            "todos": [
                {"content": "设计卡片", "activeForm": "正在设计卡片", "status": "in_progress"},
                {"content": "编写测试", "status": "pending"},
                {"content": "更新文档", "status": "completed"}
            ]
        }),
        None,
    );

    let TuiToolPresentation::Todo(todo) = presentation else {
        panic!("expected Todo presentation");
    };
    assert!(todo.is_initial);
    assert_eq!((todo.completed_count, todo.total_count), (1, 3));
    assert_eq!(todo.current_items[0].status, TuiTodoStatus::InProgress);
    assert_eq!(
        todo.changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        vec![
            TuiTodoChangeKind::Added,
            TuiTodoChangeKind::Added,
            TuiTodoChangeKind::Added
        ],
    );
}

#[test]
fn todo_diff_tracks_status_active_form_and_removed_items() {
    let previous = TodoSnapshot::parse(&json!({
        "todos": [
            {"content": "实现卡片", "activeForm": "正在实现卡片", "status": "in_progress"},
            {"content": "写测试", "status": "pending"},
            {"content": "删除我", "status": "completed"}
        ]
    }))
    .expect("previous todo snapshot should parse");

    let presentation = presentation_for(
        "TodoWrite",
        &json!({
            "todos": [
                {"content": "实现卡片", "activeForm": "正在完成卡片", "status": "completed"},
                {"content": "写测试", "status": "in_progress"},
                {"content": "新增任务", "status": "pending"}
            ]
        }),
        Some(&previous),
    );

    let TuiToolPresentation::Todo(todo) = presentation else {
        panic!("expected Todo presentation");
    };
    assert!(!todo.is_initial);
    assert_eq!((todo.completed_count, todo.total_count), (1, 3));
    assert_eq!(
        todo.changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        vec![
            TuiTodoChangeKind::Completed,
            TuiTodoChangeKind::ActiveFormUpdated,
            TuiTodoChangeKind::Started,
            TuiTodoChangeKind::Added,
            TuiTodoChangeKind::Removed,
        ],
    );
}

#[test]
fn todo_diff_marks_completed_task_returning_to_work_as_reopened() {
    let previous = TodoSnapshot::parse(&json!({
        "todos": [{"content": "回归任务", "status": "completed"}]
    }))
    .expect("previous todo snapshot should parse");

    let presentation = presentation_for(
        "TodoWrite",
        &json!({
            "todos": [{"content": "回归任务", "status": "in_progress"}]
        }),
        Some(&previous),
    );

    let TuiToolPresentation::Todo(todo) = presentation else {
        panic!("expected Todo presentation");
    };
    assert_eq!(todo.changes[0].kind, TuiTodoChangeKind::Reopened);
}

#[test]
fn identical_todo_snapshot_has_no_changes() {
    let input = json!({
        "todos": [{"content": "不变任务", "status": "pending"}]
    });
    let previous = TodoSnapshot::parse(&input).expect("snapshot should parse");

    let presentation = presentation_for("TodoWrite", &input, Some(&previous));
    let TuiToolPresentation::Todo(todo) = presentation else {
        panic!("expected Todo presentation");
    };
    assert!(todo.changes.is_empty());
}
