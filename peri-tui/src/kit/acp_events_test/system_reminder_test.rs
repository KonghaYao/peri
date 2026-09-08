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
        matches!(decoded, AcpEventData::SystemReminder { reminder, replay: false } if reminder.kind == "mode_changed")
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
fn system_reminder_fallback_is_legacy_only_and_each_path_renders_once() {
    let mut state = make_fold_test_state();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
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
fn trusted_structured_dispatch_preserves_required_and_renders_marker() {
    let mut state = make_fold_test_state();
    let _ = dispatch_trusted_structured_for_bridge(
        &mut state,
        AcpEventData::SystemReminder {
            reminder: reminder(),
            replay: false,
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
