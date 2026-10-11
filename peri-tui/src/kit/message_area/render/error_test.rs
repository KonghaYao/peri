use super::*;
use crate::kit::message_area::render::{vm_to_lines, vm_to_lines_cached};
use crate::kit::tui_render_unit::{FoldState, TuiSubAgentGroup, TuiSystemNote, TuiToolCard};

fn note(text: &str, level: TuiNoteLevel) -> TuiRenderUnit {
    TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: text.into(),
        level,
        content_hash: 17,
    })
}

fn tool(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "error-call".into(),
        tool_name: "Bash".into(),
        input_summary: "command".into(),
        output_summary: text.into(),
        is_error: true,
        fold: FoldState::Expanded,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: Default::default(),
        user_modified: false,
        content_hash: 23,
        tool_calls_count: 0,
    })
}

fn group(children: Vec<TuiRenderUnit>, reason: Option<&str>) -> TuiRenderUnit {
    TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        instance_id: "occurrence".into(),
        agent_id: "child".into(),
        agent_name: "Child".into(),
        view_models: children.into(),
        collapsed: false,
        is_running: false,
        is_error: true,
        error_reason: reason.map(str::to_owned),
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: 19,
    })
}

#[test]
fn error_preview_wraps_instead_of_truncating_first_line() {
    assert_eq!(
        preview_lines("abcdefghijkl", 4, 3),
        ["abcd", "efgh", "ijkl"]
    );
}

#[test]
fn error_preview_limits_rows_with_an_ellipsis() {
    let lines = preview_lines(&"long error ".repeat(10_000), 10, 3);
    assert_eq!(lines.len(), 3);
    assert!(lines.last().unwrap().ends_with('…'));
    assert!(lines.iter().all(|line| line.width() <= 10));
}

#[test]
fn error_preview_preserves_graphemes_and_explicit_newlines() {
    assert_eq!(
        preview_lines("界界\n👩‍💻e\u{301}", 4, 3),
        ["界界", "👩‍💻e\u{301}"]
    );
    assert_eq!(
        preview_lines("one\r\ntwo\r\nthree\r\n", 8, 3),
        ["one", "two", "three"]
    );
    assert_eq!(
        preview_lines("one\ntwo\nthree\nfour", 8, 3),
        ["one", "two", "three…"]
    );
    assert!(
        preview_lines("界界", 1, 3)
            .iter()
            .all(|line| line.width() <= 1)
    );
}

#[test]
fn error_preview_handles_zero_width_and_empty_text() {
    assert!(preview_lines("error", 0, 3).is_empty());
    assert!(preview_lines("error", 10, 0).is_empty());
    assert_eq!(preview_lines("", 10, 3), [""]);
}

#[test]
fn system_error_icon_is_a_copy_target_even_on_narrow_terminals() {
    let text = "HTTP 400: ".to_owned() + &"long 中文 response ".repeat(100);
    let vm = note(&text, TuiNoteLevel::Error);
    for width in [12, 30, 80, 120] {
        let grid = GridSpec::grid_for(width);
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        let (lines, button, _, _) = vm_to_lines_cached(&vm, &grid, &mut cache, true);
        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|line| line.width() <= grid.first_prefix_width() + grid.content_width())
        );
        let button = button.unwrap();
        assert_eq!(button.logical_idx, 0);
        assert_eq!((button.x_start, button.x_end), (grid.outer, grid.outer + 1));
        assert_eq!(
            lines[0].spans[1].content,
            super::super::helpers::sym().error
        );
        assert_eq!(
            copy_text_at(&vm, button.logical_idx).as_deref(),
            Some(text.as_str())
        );
    }
}

#[test]
fn warning_and_info_notes_are_not_copy_targets() {
    for level in [TuiNoteLevel::Warning, TuiNoteLevel::Info] {
        let vm = note("not an error", level);
        assert!(copy_text_at(&vm, 0).is_none());
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        assert!(
            vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true)
                .1
                .is_none()
        );
    }
}

#[test]
fn tool_error_preview_is_three_rows_and_copy_is_not_clipped() {
    let text = "first line\n".to_owned() + &"detailed error ".repeat(50);
    let vm = tool(&text);
    let grid = GridSpec::grid_for(80);
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 3);
    assert_eq!(copy_text_at(&vm, 0).as_deref(), Some(text.as_str()));
}

#[test]
fn subagent_error_icons_copy_their_own_complete_error() {
    let first = "older child error ".repeat(40);
    let second = "newer child error ".repeat(40);
    let parent = "parent failure\n".to_owned() + &"cause ".repeat(40);
    let vm = group(vec![tool(&first), tool(&second)], Some(&parent));
    let grid = GridSpec::grid_for(80);
    let buttons = subagent_error_buttons(&vm, &grid);
    assert_eq!(buttons.len(), 3);
    assert_eq!(
        copy_text_at(&vm, buttons[0].logical_idx).as_deref(),
        Some(second.as_str())
    );
    assert_eq!(
        copy_text_at(&vm, buttons[1].logical_idx).as_deref(),
        Some(first.as_str())
    );
    assert_eq!(
        copy_text_at(&vm, buttons[2].logical_idx).as_deref(),
        Some(parent.as_str())
    );
    assert_eq!(vm_to_lines(&vm, &grid).len(), 5);
}

#[test]
fn subagent_fallback_keeps_the_entire_error_for_copy() {
    let text = "first error line\nsecond error line\nthird error line\nfourth";
    let vm = group(vec![tool(text)], None);
    let buttons = subagent_error_buttons(&vm, &GridSpec::grid_for(80));
    assert_eq!(buttons.len(), 2);
    assert_eq!(
        copy_text_at(&vm, buttons[1].logical_idx).as_deref(),
        Some(text)
    );
}

#[test]
fn successful_subagent_keeps_child_error_local_and_ignores_stale_parent_reason() {
    let mut vm = group(vec![tool("child error")], Some("stale parent reason"));
    if let TuiRenderUnit::TuiSubAgentGroup(data) = &mut vm {
        data.is_error = false;
    }
    let grid = GridSpec::grid_for(80);
    let buttons = subagent_error_buttons(&vm, &grid);
    assert_eq!(buttons.len(), 1);
    assert_eq!(vm_to_lines(&vm, &grid).len(), 1);
    assert_eq!(copy_text_at(&vm, 0).as_deref(), Some("child error"));
    assert!(copy_text_at(&vm, 1).is_none());
}
