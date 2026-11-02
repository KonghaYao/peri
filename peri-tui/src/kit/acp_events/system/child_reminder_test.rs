use super::*;
use crate::kit::acp_events::acp_events_test::make_fold_test_state;
use crate::kit::acp_types::subagent_preview::{DETAIL_BYTES, FIELD_BYTES, STORE_BYTES};
use crate::kit::atoms::{
    BG_AGENT_IDS, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASK_REVISION, BG_TASKS,
    BgDisplayEntry, BgLiveDetail, BgTaskIdentity,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SYSTEM_REMINDER_VERSION, SystemReminder,
};
use serial_test::serial;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn reminder() -> SystemReminder {
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Task,
        source: ReminderSource("child_task".into()),
        kind: "update".into(),
        severity: ReminderSeverity::Info,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
        body: "x".repeat(64 * 1024),
        summary: Some("child update".into()),
        metadata: serde_json::json!({"detail": "m".repeat(8 * 1024)}),
    };
    reminder
        .validate()
        .expect("valid structured reminder fixture");
    reminder
}

fn deliver(state: &mut BridgeState, value: &SystemReminder, child: Option<&str>, trusted: bool) {
    if trusted {
        handle_trusted_system_reminder(state, value.clone(), child);
    } else {
        handle_system_reminder(state, value, child);
    }
}

#[test]
#[serial]
fn child_reminder_ingress_bounds_only_the_display_copy_and_preserves_policy() {
    let original = reminder();
    for trusted in [false, true] {
        let mut state = make_fold_test_state();
        let _restore = RestoreBackground::empty();
        state.current_turn.append_text("parent output", None);
        state
            .current_turn
            .start_subagent("child".into(), "coder".into(), None);
        deliver(&mut state, &original, Some("child"), trusted);
        assert!(state.committed.is_empty());
        assert_eq!(state.current_turn.text, "parent output");
        let child = &mut state.current_turn.subagents[0];
        assert!(child.preview_omitted);
        assert!(child.preview_reserved() <= DETAIL_BYTES);
        let TuiRenderUnit::TuiSubAgentGroup(group) = child.view_model() else {
            unreachable!()
        };
        let TuiRenderUnit::TuiSystemReminder(preview) = &group.view_models[0] else {
            unreachable!()
        };
        assert_eq!(preview.body.len(), FIELD_BYTES);
        assert_eq!(preview.required, trusted);
        assert_eq!(preview.wire.as_ref().unwrap().audiences, original.audiences);
        assert_eq!(preview.relay.as_ref().unwrap().label(), "coder");
        let reserved = crate::kit::retained_bytes::retained_reminder_bytes(preview);
        assert!(reserved >= preview.body.capacity() * 2 + 8 * 1024);
        drop(group);
        for _ in 0..12 {
            deliver(&mut state, &original, Some("child"), trusted);
            assert!(
                state.current_turn.subagents[0].preview_reserved() <= DETAIL_BYTES,
                "the post-publication cache must fit too, not just raw input"
            );
        }
    }
    assert_eq!(original.body.len(), 64 * 1024);
    assert_eq!(original.delivery, ReminderDelivery::Required);

    let mut root = make_fold_test_state();
    deliver(&mut root, &original, None, true);
    let TuiRenderUnit::TuiSystemReminder(root_reminder) = &root.committed[0] else {
        unreachable!()
    };
    assert_eq!(root_reminder.body, original.body);
    assert_eq!(root_reminder.wire.as_ref(), Some(&original));
    assert!(root_reminder.required);
    assert!(root_reminder.relay.is_none());
}

#[test]
#[serial]
fn child_reminder_rejects_unknown_stopped_invalid_and_non_tui_inputs() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    let mut value = reminder();
    for trusted in [false, true] {
        deliver(&mut state, &value, Some("unknown"), trusted);
    }
    assert!(state.committed.is_empty());
    state
        .current_turn
        .start_subagent("child".into(), "coder".into(), None);
    value.audiences = ReminderAudiences(vec![ReminderAudience::Model]);
    deliver(&mut state, &value, Some("child"), true);
    value.audiences = ReminderAudiences(vec![ReminderAudience::Tui]);
    value.body.push('!'); // Clipping must not turn an invalid original into valid input.
    deliver(&mut state, &value, Some("child"), false);
    assert_eq!(
        state.current_turn.subagents[0]
            .child_turn
            .view_model_count(),
        0
    );
    state.current_turn.stop_subagent("child", false, "done");
    let before = state.current_turn.subagents[0].view_model();
    for index in 0..1000 {
        deliver(&mut state, &reminder(), Some("child"), index % 2 == 0);
    }
    assert_eq!(state.current_turn.subagents[0].view_model(), before);
    assert!(state.committed.is_empty());
}

#[test]
#[serial]
fn child_reminder_long_parent_and_repeated_terminals_do_not_accumulate() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    let value = reminder();
    let mut first_body = None;
    for index in 0..1000 {
        let id = format!("reminder-child-{index}");
        state
            .current_turn
            .start_subagent(id.clone(), "coder".into(), None);
        state
            .current_turn
            .append_subagent_text(&id, "body before reminder");
        deliver(&mut state, &value, Some(&id), index % 2 == 0);
        let child = state
            .current_turn
            .subagents
            .iter_mut()
            .find(|child| child.agent_id == id)
            .unwrap();
        let TuiRenderUnit::TuiSubAgentGroup(group) = child.view_model() else {
            unreachable!()
        };
        assert!(
            group
                .view_models
                .iter()
                .any(|unit| matches!(unit, TuiRenderUnit::TuiSystemReminder(_))),
            "the retention regression must exercise admitted reminder content"
        );
        drop(group);
        state.current_turn.stop_subagent(&id, false, "done");
        crate::kit::acp_events::push_view_models(&mut state);
        if index == 0 {
            let TuiRenderUnit::TuiSubAgentGroup(group) =
                state.current_turn.subagents[0].view_model()
            else {
                unreachable!()
            };
            first_body = group.view_models.iter().find_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(body) => Some(Arc::downgrade(body)),
                _ => None,
            });
        }
        state.current_turn.stop_subagent(&id, false, "duplicate");
        deliver(&mut state, &value, Some(&id), true);
        assert!(state.committed.is_empty());
        assert!(
            state
                .current_turn
                .subagents
                .iter()
                .all(|child| child.preview_reserved() <= DETAIL_BYTES)
        );
    }
    assert!(state.current_turn.subagents.len() <= 256);
    assert!(
        state
            .current_turn
            .subagents
            .iter()
            .map(|child| child.preview_reserved())
            .sum::<usize>()
            <= STORE_BYTES
    );
    assert!(first_body.unwrap().upgrade().is_none());
    assert!(state.memory_snapshot().unique_reserved_bytes < 2 * 1024 * 1024);
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn publication_enforces_each_child_limit_even_without_an_append_helper() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    state
        .current_turn
        .start_subagent("direct".into(), "coder".into(), None);
    for _ in 0..4 {
        state.current_turn.subagents[0]
            .child_turn
            .push_system_reminder(TuiSystemReminder::from_wire(reminder()).unwrap());
    }
    assert!(state.current_turn.subagents[0].preview_reserved() > DETAIL_BYTES);
    assert!(state.current_turn.subagents[0].preview_reserved() < STORE_BYTES);
    crate::kit::acp_events::push_view_models(&mut state);
    assert!(state.current_turn.subagents[0].preview_reserved() <= DETAIL_BYTES);
    assert!(state.current_turn.subagents[0].preview_omitted);
    assert!(state.current_turn.subagents[0].is_running);
    crate::kit::acp_events::push_view_models_for_reset();
}

struct RestoreBackground {
    tasks: Vec<crate::kit::acp_types::BgTaskEntry>,
    display: Vec<BgDisplayEntry>,
    details: HashMap<String, BgLiveDetail>,
    identities: HashMap<String, BgTaskIdentity>,
    agents: HashSet<String>,
    revision: Option<u64>,
}
impl RestoreBackground {
    fn empty() -> Self {
        Self {
            tasks: std::mem::take(&mut *BG_TASKS.state().write()),
            display: std::mem::take(&mut *BG_DISPLAY.state().write()),
            details: std::mem::take(&mut *BG_LIVE_DETAIL.state().write()),
            identities: std::mem::take(&mut *BG_TASK_IDENTITY.state().write()),
            agents: std::mem::take(&mut *BG_AGENT_IDS.state().write()),
            revision: BG_TASK_REVISION.state().write().take(),
        }
    }
}
impl Drop for RestoreBackground {
    fn drop(&mut self) {
        *BG_TASKS.state().write() = std::mem::take(&mut self.tasks);
        *BG_DISPLAY.state().write() = std::mem::take(&mut self.display);
        *BG_LIVE_DETAIL.state().write() = std::mem::take(&mut self.details);
        *BG_TASK_IDENTITY.state().write() = std::mem::take(&mut self.identities);
        *BG_AGENT_IDS.state().write() = std::mem::take(&mut self.agents);
        BG_TASK_REVISION.set(self.revision);
    }
}

#[test]
#[serial]
fn background_child_reminder_uses_its_detail_budget_and_keeps_independent_shell() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    for (id, kind) in [("bg-agent-task", "agent"), ("independent-shell", "shell")] {
        handle_bg_task_started(
            &mut state,
            &crate::kit::acp_types::BgTaskEntry {
                task_id: id.into(),
                kind: kind.into(),
                summary: "running".into(),
                started_at: String::new(),
                pid: None,
                revision: None,
                status: Some("running".into()),
            },
        );
    }
    crate::kit::acp_events::subagent::handle_subagent_started(
        &mut state, "bg-child", "coder", true, None,
    );
    BG_LIVE_DETAIL
        .state()
        .write()
        .get_mut("independent-shell")
        .unwrap()
        .output_preview = Some("shell output".into());
    crate::kit::bg_task_live::append_bg_text_chunk(
        "bg-child",
        &crate::kit::stream_data::TuiTextChunk {
            text: "before reminder".into(),
            message_id: Some("before-reminder".into()),
            agent_id: Some("bg-child".into()),
        },
    );
    let value = reminder();
    for index in 0..1000 {
        deliver(&mut state, &value, Some("bg-child"), index % 2 == 0);
        if index == 0 {
            let live = BG_LIVE_DETAIL.state();
            let details = live.read();
            let units = &details["bg-agent-task"].nested_units;
            assert!(
                matches!(&units[0], TuiRenderUnit::TuiAssistantBubble(body) if body.text == "before reminder")
            );
            assert!(matches!(&units[1], TuiRenderUnit::TuiSystemReminder(_)));
        }
    }
    assert!(state.committed.is_empty());
    {
        let details = BG_LIVE_DETAIL.state();
        let details = details.read();
        assert!(details["bg-agent-task"].preview_reserved() <= DETAIL_BYTES);
        assert!(details["bg-agent-task"].preview_omitted);
        assert!(
            details["bg-agent-task"]
                .nested_units
                .iter()
                .any(|unit| matches!(unit, TuiRenderUnit::TuiSystemReminder(_)))
        );
        assert_eq!(
            details["independent-shell"].status,
            crate::kit::atoms::BgLiveStatus::Running
        );
    }
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "independent-shell")
    );
    let shell_before = BG_LIVE_DETAIL.state().read()["independent-shell"].clone();
    crate::kit::acp_events::subagent::handle_subagent_stopped(
        &mut state, "bg-child", "done", false,
    );
    let child_units = BG_LIVE_DETAIL.state().read()["bg-agent-task"]
        .nested_units
        .clone();
    deliver(&mut state, &value, Some("bg-child"), true);
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["bg-agent-task"].nested_units,
        child_units
    );
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["independent-shell"].nested_units,
        shell_before.nested_units
    );
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["independent-shell"].status,
        shell_before.status
    );
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["independent-shell"]
            .output_preview
            .as_deref(),
        Some("shell output")
    );
}
