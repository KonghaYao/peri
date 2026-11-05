//! Pure policy tests for monitor alert delivery.

use std::time::{Duration, Instant};

use peri_acp_types::monitor::{
    monitor_alert_delivery_id, monitor_alert_reminder, MONITOR_META_KEY,
};
use rmcp::model::{JsonObject, MetaObject};
use serde_json::json;

use super::*;

fn meta_with(value: serde_json::Value) -> MetaObject {
    let mut fields = JsonObject::new();
    fields.insert(MONITOR_META_KEY.to_string(), value);
    MetaObject(fields)
}

fn valid_meta() -> MetaObject {
    meta_with(json!({
        "alertMatched": 3,
        "lastAlertSeq": 42,
        "label": "build"
    }))
}

/// Mirror the production loop: metadata parse failure skips the decision, so
/// the per-task state is never advanced.
fn observe(
    state: &mut MonitorAlertState,
    meta: Option<&MetaObject>,
    now: Instant,
    has_inbox: bool,
) -> Result<MonitorAlertAction, &'static str> {
    let meta = parse_monitor_meta(meta)?;
    Ok(state.decide(meta.alert_matched, now, has_inbox))
}

#[test]
fn first_observation_with_hits_delivers_immediately() {
    let mut state = MonitorAlertState::default();
    let now = Instant::now();
    assert_eq!(
        state.decide(3, now, true),
        MonitorAlertAction::Send { hits: 3 },
        "processed starts at zero, so the first cumulative count is delivered"
    );
    assert_eq!(state.processed(), 3);
    assert_eq!(state.next_allowed(), Some(now + MONITOR_ALERT_MERGE_WINDOW));
}

#[test]
fn hits_inside_the_window_are_suppressed_without_advancing() {
    let mut state = MonitorAlertState::default();
    let start = Instant::now();
    assert_eq!(
        state.decide(1, start, true),
        MonitorAlertAction::Send { hits: 1 }
    );
    let inside = start + MONITOR_ALERT_MERGE_WINDOW - Duration::from_secs(1);
    assert_eq!(
        state.decide(4, inside, true),
        MonitorAlertAction::Suppressed
    );
    assert_eq!(
        state.processed(),
        1,
        "suppressed hits are not marked processed"
    );
}

#[test]
fn hits_after_the_window_deliver_the_cumulative_count() {
    let mut state = MonitorAlertState::default();
    let start = Instant::now();
    assert_eq!(
        state.decide(1, start, true),
        MonitorAlertAction::Send { hits: 1 }
    );
    let due = start + MONITOR_ALERT_MERGE_WINDOW;
    assert_eq!(
        state.decide(4, due, true),
        MonitorAlertAction::Send { hits: 4 },
        "the reminder reports the cumulative count, not the delta"
    );
    assert_eq!(state.processed(), 4);
}

#[test]
fn terminal_take_returns_the_unreported_delta_once() {
    let mut state = MonitorAlertState::default();
    assert_eq!(
        state.take_unreported(3),
        3,
        "no alert was delivered before the terminal observation"
    );
    assert_eq!(state.processed(), 3);
    assert_eq!(
        state.take_unreported(3),
        0,
        "a retried terminal observation must not reuse the same hits"
    );
    assert_eq!(state.take_unreported(5), 2);
    assert_eq!(state.processed(), 5);
}

#[test]
fn terminal_take_ignores_a_lower_cumulative_count() {
    let mut state = MonitorAlertState::default();
    state.decide(4, Instant::now(), true);
    assert_eq!(
        state.take_unreported(2),
        0,
        "the cumulative count cannot go backwards"
    );
    assert_eq!(state.processed(), 4);
}

#[test]
fn offline_owner_drops_and_advances() {
    let mut state = MonitorAlertState::default();
    let start = Instant::now();
    assert_eq!(
        state.decide(4, start, false),
        MonitorAlertAction::DropOffline { hits: 4 }
    );
    assert_eq!(state.processed(), 4, "an offline drop is final");
    assert_eq!(
        state.next_allowed(),
        None,
        "an offline drop opens no window"
    );

    assert_eq!(
        state.decide(5, start + Duration::from_secs(1), true),
        MonitorAlertAction::Send { hits: 5 },
        "later hits are still delivered once an inbox exists"
    );
}

#[test]
fn missing_or_malformed_meta_is_an_error_and_does_not_advance() {
    let mut state = MonitorAlertState::default();
    let now = Instant::now();
    assert!(observe(&mut state, None, now, true).is_err());
    assert!(observe(&mut state, Some(&MetaObject::new()), now, true).is_err());
    let malformed = meta_with(json!({
        "alertMatched": "three",
        "lastAlertSeq": 42,
        "label": null
    }));
    assert!(observe(&mut state, Some(&malformed), now, true).is_err());
    assert_eq!(state.processed(), 0);
    assert_eq!(state.next_allowed(), None);
}

#[test]
fn valid_meta_parses_camel_case_fields() {
    let parsed = parse_monitor_meta(Some(&valid_meta())).expect("valid meta");
    assert_eq!(parsed.alert_matched, 3);
    assert_eq!(parsed.last_alert_seq, Some(42));
    assert_eq!(parsed.label.as_deref(), Some("build"));
}

#[test]
fn same_id_and_hits_produce_verbatim_identical_reminders() {
    let first = monitor_alert_reminder("monitor-1", Some("build"), 7, Some(99))
        .expect("reminder")
        .as_reminder()
        .body
        .clone();
    let second = monitor_alert_reminder("monitor-1", Some("build"), 7, Some(99))
        .expect("reminder")
        .as_reminder()
        .body
        .clone();
    assert_eq!(first, second);
    assert_eq!(
        monitor_alert_delivery_id("monitor-1", 7),
        monitor_alert_delivery_id("monitor-1", 7)
    );
    assert_ne!(
        monitor_alert_delivery_id("monitor-1", 7),
        monitor_alert_delivery_id("monitor-1", 8)
    );
}
