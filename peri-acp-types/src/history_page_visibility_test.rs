use crate::messages::BaseMessage;
use crate::messages::MessageId;
use crate::store;
use crate::store::PersistedPayload;
use crate::system_reminder::SystemReminder;
use crate::system_reminder::TrustedSystemReminderFactory;
use crate::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SYSTEM_REMINDER_VERSION,
};

#[test]
fn history_body_visibility_matches_regular_payload_projection() {
    let mut payloads = vec![
        PersistedPayload::Message(BaseMessage::system("hidden".repeat(4096))),
        PersistedPayload::Message(BaseMessage::human("visible".repeat(4096))),
        PersistedPayload::Message(BaseMessage::ai("visible")),
        PersistedPayload::Message(BaseMessage::tool_result("call", "result")),
    ];
    for audience in [
        ReminderAudience::Model,
        ReminderAudience::Tui,
        ReminderAudience::Diagnostics,
    ] {
        for delivery in [
            ReminderDelivery::Required,
            ReminderDelivery::Configurable,
            ReminderDelivery::DiagnosticOnly,
        ] {
            let reminder = SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Security,
                source: ReminderSource("visibility_test".into()),
                kind: "notice".into(),
                severity: ReminderSeverity::Warning,
                delivery,
                audiences: ReminderAudiences(vec![audience]),
                body: "正文".repeat(4096),
                summary: None,
                metadata: serde_json::json!({}),
            };
            payloads.push(PersistedPayload::SystemReminder {
                id: MessageId::new(),
                reminder: TrustedSystemReminderFactory::for_producer()
                    .construct(reminder)
                    .unwrap(),
            });
        }
    }
    for payload in payloads {
        let encoded = store::serialize_persisted_payload(&payload).unwrap();
        assert_eq!(
            crate::history_page_visibility::history_body_displayable_to_client(encoded.as_bytes())
                .unwrap(),
            payload.displayable_to_client()
        );
        if let PersistedPayload::Message(message) = payload {
            let legacy = serde_json::to_vec(&message).unwrap();
            assert_eq!(
                crate::history_page_visibility::history_body_displayable_to_client(
                    legacy.as_slice()
                )
                .unwrap(),
                !message.is_system()
            );
        }
        assert_eq!(
            store::deserialize_persisted_payload(&encoded)
                .unwrap()
                .displayable_to_client(),
            crate::history_page_visibility::history_body_displayable_to_client(encoded.as_bytes())
                .unwrap()
        );
    }
}

#[test]
fn history_body_visibility_rejects_unknown_envelopes_and_truncated_json() {
    for encoded in [
        r#"{"version":2,"type":"message","message":{"role":"user"}}"#,
        r#"{"version":1,"type":"unknown"}"#,
        "{}",
    ] {
        assert!(
            !crate::history_page_visibility::history_body_displayable_to_client(encoded.as_bytes())
                .unwrap()
        );
    }
    assert!(
        crate::history_page_visibility::history_body_displayable_to_client(
            br#"{"role":"user","content":"unfinished"#.as_slice()
        )
        .is_err()
    );
}
