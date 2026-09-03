use super::*;
use crate::kit::atoms::{BG_AGENT_IDS, BgDisplayEntry};
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;
use std::time::Instant;

fn reset_route_atoms() {
    crate::kit::atoms::init_atoms();
    BG_AGENT_IDS.state().write().clear();
}

fn sample_entry(id: &str, kind: &str) -> BgDisplayEntry {
    BgDisplayEntry {
        id: id.into(),
        linked_agent_id: None,
        agent_type: kind.into(),
        desc: "d".into(),
        current_tool: None,
        tool_count: 0,
        is_active: true,
        is_error: false,
        created_at: Instant::now(),
        completed_at: None,
    }
}

#[test]
#[serial]
fn test_route_bg_task_click_agent_shell_workflow() {
    reset_route_atoms();
    let agent = sample_entry("t-agent", "agent");
    assert_eq!(
        route_bg_task_click(&agent),
        BgTaskClickRoute::SubAgent {
            subagent_id: "t-agent".into()
        }
    );

    let shell = sample_entry("t-shell", "shell");
    assert_eq!(
        route_bg_task_click(&shell),
        BgTaskClickRoute::Shell {
            task_id: "t-shell".into()
        }
    );

    let wf = sample_entry("run-1", "workflow");
    assert_eq!(
        route_bg_task_click(&wf),
        BgTaskClickRoute::Workflow {
            run_id: "run-1".into()
        }
    );
}

#[test]
#[serial]
fn test_route_bg_task_click_unknown_kind_ignored() {
    reset_route_atoms();
    let bad = sample_entry("x", "cron");
    assert_eq!(route_bg_task_click(&bad), BgTaskClickRoute::UnknownKind);
}

#[test]
fn test_bg_task_hit_row_maps_sorted_index() {
    let e0 = sample_entry("a", "agent");
    let e1 = sample_entry("b", "shell");
    let sorted = sort_bg_display_rows(vec![&e0, &e1]);
    let area = Rect::new(0, 10, 80, 2);
    let hits = build_bg_task_line_hits(area, &sorted);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].row, 10);
    assert_eq!(hits[0].sorted_index, 0);
    assert_eq!(hits[1].row, 11);
    assert_eq!(hits[1].sorted_index, 1);

    let hit0 = hit_test_bg_task_line(&hits, area, 5, 10).expect("row 0");
    assert_eq!(hit0.task_id, "a");
    let hit1 = hit_test_bg_task_line(&hits, area, 79, 11).expect("row 1");
    assert_eq!(hit1.task_id, "b");
    assert!(hit_test_bg_task_line(&hits, area, 5, 12).is_none());
    assert!(hit_test_bg_task_line(&hits, area, 80, 10).is_none());
}

#[test]
fn test_bg_task_area_empty_height_zero() {
    let now = Instant::now();
    let visible = visible_bg_display_entries(&[], now);
    assert_eq!(visible.len(), 0);
    assert_eq!(
        visible.len() as u16,
        0,
        "empty BG_DISPLAY → render height 0"
    );
}

#[test]
fn test_completed_row_drops_from_visible_bar_after_keep_secs() {
    let now = Instant::now();
    let mut entry = sample_entry("old", "shell");
    entry.is_active = false;
    entry.completed_at = Some(now - std::time::Duration::from_secs(DONE_KEEP_SECS + 1));
    let visible = visible_bg_display_entries(std::slice::from_ref(&entry), now);
    assert!(
        visible.is_empty(),
        "3s 后底栏行隐藏，但不删 BG_DISPLAY / 不关抽屉"
    );
}
