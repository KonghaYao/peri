//! Tests for shell_detail panel helpers.
use super::*;
use crate::kit::atoms::{BgDisplayEntry, BgLiveDetail, BgLiveStatus};
use serial_test::serial;
use std::collections::HashMap;
use std::time::Instant;

#[test]
fn test_shell_detail_not_found_i18n_key_resolves() {
    let msg = i18n::tr("shell-detail-not-found");
    assert!(!msg.is_empty());
    assert!(!msg.contains("shell-detail-not-found"));
}

#[test]
fn test_output_section_uses_preview_when_present() {
    let live = BgLiveDetail {
        status: BgLiveStatus::Succeeded,
        output_preview: Some("done".to_string()),
        ..Default::default()
    };
    match output_section(Some(&live), None) {
        OutputSection::Preview(p) => assert_eq!(p, "done"),
        _ => panic!("expected preview"),
    }
}

#[test]
fn test_resolve_shell_detail_context_after_removed_from_bg_tasks() {
    let mut live = HashMap::new();
    live.insert(
        "s-done".to_string(),
        BgLiveDetail {
            status: BgLiveStatus::Succeeded,
            kind: "shell".to_string(),
            summary: "echo done".to_string(),
            output_preview: Some("ok".to_string()),
            ..Default::default()
        },
    );
    let display = vec![BgDisplayEntry {
        id: "s-done".into(),
        linked_agent_id: None,
        agent_type: "shell".into(),
        desc: "echo done".into(),
        current_tool: None,
        tool_count: 0,
        is_active: false,
        is_error: false,
        created_at: Instant::now(),
        completed_at: Some(Instant::now()),
    }];
    let ctx = resolve_shell_detail_context(&[], &display, &live, Some("s-done"))
        .expect("completed shell still resolvable");
    assert_eq!(ctx.summary, "echo done");
}

#[test]
#[serial]
fn test_build_shell_lines_non_empty_when_only_live_projection() {
    crate::kit::atoms::init_atoms();
    let mut live = HashMap::new();
    live.insert(
        "s1".to_string(),
        BgLiveDetail {
            status: BgLiveStatus::Succeeded,
            kind: "shell".to_string(),
            summary: "curl example".to_string(),
            output_preview: Some("200 OK".to_string()),
            ..Default::default()
        },
    );
    let theme = peri_theme::atoms::THEME_ATOM.state().read().clone();
    let lines = build_shell_detail_lines(Some("s1"), &[], &[], &live, theme.as_ref());
    assert!(
        !lines.is_empty(),
        "drawer should stay populated after bar row expires"
    );
}

#[test]
fn test_output_section_running_without_preview() {
    let live = BgLiveDetail {
        status: BgLiveStatus::Running,
        ..Default::default()
    };
    assert!(matches!(
        output_section(Some(&live), None),
        OutputSection::RunningNoStream
    ));
}
