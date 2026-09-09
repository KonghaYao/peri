use peri_acp_types::system_reminder::{
    decode_system_reminder_json, encode_system_reminder, ReminderAudience, ReminderAudiences,
    ReminderCategory, ReminderDelivery, ReminderFilter, ReminderSeverity, ReminderSource,
    SystemReminder, TrustedReminderProvenance, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use serde_json::json;

fn sample() -> SystemReminder {
    SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("permission".into()),
        kind: "mode_changed".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Model]),
        body: "mode changed".into(),
        summary: None,
        metadata: json!({}),
    }
}

#[test]
fn downstream_producer_can_explicitly_assert_provenance_and_use_control_apis() {
    let trusted = TrustedSystemReminderFactory::for_producer()
        .construct(sample())
        .unwrap();

    assert_eq!(trusted.provenance(), TrustedReminderProvenance::Producer);
    assert!(ReminderFilter::default().allows(&trusted, ReminderAudience::Model));
    assert!(encode_system_reminder(&trusted).is_ok());
}

#[test]
fn downstream_recovery_can_explicitly_assert_recovery_provenance() {
    let trusted = TrustedSystemReminderFactory::for_recovery()
        .construct(sample())
        .unwrap();

    assert_eq!(trusted.provenance(), TrustedReminderProvenance::Recovery);
}

#[test]
fn wire_decode_returns_only_untrusted_protocol_data() {
    let wire = serde_json::to_vec(&sample()).unwrap();
    let decoded: SystemReminder = decode_system_reminder_json(&wire).unwrap();

    assert_eq!(decoded, sample());
    assert!(serde_json::to_value(decoded)
        .unwrap()
        .get("provenance")
        .is_none());
}
