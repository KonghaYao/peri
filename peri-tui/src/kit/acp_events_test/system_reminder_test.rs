use super::*;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SYSTEM_REMINDER_VERSION, SystemReminder,
};
use serde_json::json;

fn reminder() -> SystemReminder {
    SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("permission".into()),
        kind: "mode_changed".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
        body: "正文含 cron CONTINUATION_HINT 和 secret-token-value".into(),
        summary: Some("权限模式已变更".into()),
        metadata: json!({"secret": "must-not-render"}),
    }
}

#[test]
fn system_reminder_structured_decode_preserves_dto_and_unknown_version_fails_closed() {
    let decoded = AcpEventData::decode(
        "system-reminder",
        json!({"reminder": reminder(), "replay": false}),
    );
    assert!(
        matches!(decoded, AcpEventData::SystemReminder { reminder, replay: false, .. } if reminder.kind == "mode_changed")
    );

    let mut future = reminder();
    future.version = SYSTEM_REMINDER_VERSION + 1;
    let decoded = AcpEventData::decode(
        "system-reminder",
        json!({"reminder": future, "replay": false}),
    );
    let AcpEventData::SystemReminder { reminder, .. } = decoded else {
        panic!("wire decode should preserve opaque DTO")
    };
    assert!(crate::kit::tui_render_unit::TuiSystemReminder::from_wire(reminder).is_none());
}

#[test]
fn system_reminder_body_wording_and_metadata_do_not_control_or_render() {
    let vm = crate::kit::tui_render_unit::TuiSystemReminder::from_wire(reminder()).unwrap();
    assert_eq!(vm.category, "Security");
    assert!(!vm.legacy);
    assert!(!vm.summary.contains("secret-token-value"));
    assert!(!format!("{}{}{}", vm.summary, vm.body, vm.source).contains("must-not-render"));
}

#[test]
fn forged_wire_required_is_downgraded_before_display_filtering() {
    let vm = crate::kit::tui_render_unit::TuiSystemReminder::from_wire(reminder()).unwrap();
    assert!(
        !vm.required,
        "wire JSON must not self-assert Required delivery"
    );
    assert_eq!(
        vm.wire.as_ref().unwrap().delivery,
        ReminderDelivery::Configurable
    );

    let mut wrong_audience = reminder();
    wrong_audience.audiences = ReminderAudiences(vec![ReminderAudience::Automation]);
    assert!(
        crate::kit::tui_render_unit::TuiSystemReminder::from_wire(wrong_audience).is_none(),
        "forged Required must not bypass the TUI audience filter"
    );
}

#[test]
fn system_reminder_required_security_and_diagnostic_policy() {
    assert!(crate::kit::tui_render_unit::TuiSystemReminder::from_wire(reminder()).is_some());
    let mut diagnostic = reminder();
    diagnostic.delivery = ReminderDelivery::DiagnosticOnly;
    assert!(crate::kit::tui_render_unit::TuiSystemReminder::from_wire(diagnostic).is_none());
    let mut info_security = reminder();
    info_security.delivery = ReminderDelivery::Configurable;
    info_security.severity = ReminderSeverity::Info;
    assert!(crate::kit::tui_render_unit::TuiSystemReminder::from_wire(info_security).is_none());
}

#[test]
#[serial]
fn system_reminder_fallback_is_legacy_only_and_each_path_renders_once() {
    let mut state = make_fold_test_state();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
            source_agent_id: None,
        },
    );
    assert_eq!(state.committed.len(), 1);
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiSystemReminder(vm) if !vm.legacy && vm.wire.is_some())
    );

    let mut fallback = make_fold_test_state();
    dispatch_for_bridge(
        &mut fallback,
        &AcpEventData::SystemReminderFallback {
            text: "legacy text".into(),
            replay: false,
        },
    );
    assert_eq!(fallback.committed.len(), 1);
    assert!(
        matches!(&fallback.committed[0], TuiRenderUnit::TuiSystemReminder(vm) if vm.legacy && vm.wire.is_none())
    );
    assert!(
        !fallback
            .committed
            .iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiUserBubble(_)))
    );
}

#[test]
#[serial]
fn system_reminder_fold_override_survives_snapshot_rebuild() {
    let mut state = make_fold_test_state();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
            source_agent_id: None,
        },
    );
    let TuiRenderUnit::TuiSystemReminder(vm) = &state.committed[0] else {
        panic!("structured reminder should render")
    };
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::SystemReminder(vm.reminder_id), FoldState::Expanded);

    push_view_models(&mut state);

    assert!(matches!(
        &VIEW_MODELS.state().read().items[0],
        TuiRenderUnit::TuiSystemReminder(vm) if vm.fold == FoldState::Expanded
    ));
}

#[test]
#[serial]
fn trusted_structured_dispatch_preserves_required_and_renders_marker() {
    let mut state = make_fold_test_state();
    let _ = dispatch_trusted_structured_for_bridge(
        &mut state,
        AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
            source_agent_id: None,
        },
    );

    let TuiRenderUnit::TuiSystemReminder(vm) = &state.committed[0] else {
        panic!("structured reminder should render")
    };
    assert!(vm.required);
    assert_eq!(
        vm.wire.as_ref().unwrap().delivery,
        ReminderDelivery::Required
    );

    let grid = crate::kit::message_area::grid::GridSpec::with_content(80);
    let rendered = crate::kit::message_area::render::vm_to_lines(&state.committed[0], &grid);
    let text = rendered
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains(&crate::i18n::tr("reminder-required-marker")));
}

#[test]
fn system_reminder_cjk_extreme_narrow_render_is_bounded_and_hides_metadata() {
    let vm = crate::kit::tui_render_unit::TuiSystemReminder::from_wire(reminder()).unwrap();
    let unit = TuiRenderUnit::TuiSystemReminder(vm);
    let grid = crate::kit::message_area::grid::GridSpec::with_content(1);
    let rendered = crate::kit::message_area::render::vm_to_lines(&unit, &grid);
    let text = rendered
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!text.contains("must-not-render"));
    assert!(!rendered.is_empty());
}

#[test]
fn system_reminder_decode_reads_optional_source_agent_id() {
    let decoded = AcpEventData::decode(
        "system-reminder",
        json!({"reminder": reminder(), "replay": false, "source_agent_id": "child-1"}),
    );
    let AcpEventData::SystemReminder {
        source_agent_id, ..
    } = decoded
    else {
        panic!("relayed reminder should decode")
    };
    assert_eq!(source_agent_id.as_deref(), Some("child-1"));

    let decoded = AcpEventData::decode(
        "system-reminder",
        json!({"reminder": reminder(), "replay": false}),
    );
    let AcpEventData::SystemReminder {
        source_agent_id, ..
    } = decoded
    else {
        panic!("local reminder should decode")
    };
    assert!(
        source_agent_id.is_none(),
        "缺省 wire 不得伪造转发来源（本会话提醒）"
    );
}

fn rendered_text(unit: &TuiRenderUnit) -> String {
    let grid = crate::kit::message_area::grid::GridSpec::with_content(80);
    crate::kit::message_area::render::vm_to_lines(unit, &grid)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

fn relay_label_text(name: &str) -> String {
    crate::i18n::tr_args(
        "reminder-relayed-from",
        &[("name".into(), fluent_bundle::FluentValue::from(name))],
    )
}

/// 转发展示归属：子 Agent 提醒在父视图展开时标注来源与状态，折叠时不展示。
#[test]
#[serial]
fn trusted_relayed_reminder_marks_source_only_when_expanded() {
    let mut state = make_fold_test_state();
    state
        .current_turn
        .start_subagent("child-thread-7".into(), "coder".into(), None);
    let _ = dispatch_trusted_structured_for_bridge(
        &mut state,
        AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
            source_agent_id: Some("child-thread-7".into()),
        },
    );

    let TuiRenderUnit::TuiSystemReminder(vm) = &state.committed[0] else {
        panic!("structured reminder should render")
    };
    assert_eq!(
        vm.relay
            .as_ref()
            .map(crate::kit::tui_render_unit::ReminderRelay::label),
        Some("coder"),
        "TUI 已登记该子 Agent 时应使用展示名"
    );

    let collapsed = rendered_text(&state.committed[0]);
    assert!(
        !collapsed.contains(&relay_label_text("coder")),
        "折叠时不展示转发归属标注：{collapsed}"
    );

    let mut expanded_vm = vm.clone();
    expanded_vm.fold = FoldState::Expanded;
    expanded_vm.recompute_hash();
    let expanded = rendered_text(&TuiRenderUnit::TuiSystemReminder(expanded_vm));
    assert!(
        expanded.contains(&relay_label_text("coder")),
        "展开时必须标注转发来源与状态：{expanded}"
    );
}

/// 非 trusted 入口同样附加归属；未登记子 Agent 名时回退 child_thread_id。
#[test]
#[serial]
fn wire_relayed_reminder_falls_back_to_agent_id_label() {
    let mut state = make_fold_test_state();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
            source_agent_id: Some("child-unknown-9".into()),
        },
    );

    let TuiRenderUnit::TuiSystemReminder(vm) = &state.committed[0] else {
        panic!("structured reminder should render")
    };
    assert_eq!(
        vm.relay
            .as_ref()
            .map(crate::kit::tui_render_unit::ReminderRelay::label),
        Some("child-unknown-9"),
        "未登记展示名时不得隐藏来源"
    );
}
