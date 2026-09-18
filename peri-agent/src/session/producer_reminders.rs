//! Trusted System Reminder constructors for peri-agent production producers.

use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, ReminderValidationError, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub(crate) fn trusted_reminder(
    category: ReminderCategory,
    source: &str,
    kind: &str,
    severity: ReminderSeverity,
    delivery: ReminderDelivery,
    body: String,
    summary: Option<String>,
    metadata: Value,
) -> TrustedSystemReminder {
    try_trusted_reminder(
        category, source, kind, severity, delivery, body, summary, metadata,
    )
    .expect("peri-agent reminder contract must be valid")
}

/// Runtime-derived producers must handle validation failure without panicking.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_trusted_reminder(
    category: ReminderCategory,
    source: &str,
    kind: &str,
    severity: ReminderSeverity,
    delivery: ReminderDelivery,
    body: String,
    summary: Option<String>,
    metadata: Value,
) -> Result<TrustedSystemReminder, ReminderValidationError> {
    TrustedSystemReminderFactory::for_producer().construct(SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category,
        source: ReminderSource(source.into()),
        kind: kind.into(),
        severity,
        delivery,
        audiences: ReminderAudiences(vec![
            ReminderAudience::Model,
            ReminderAudience::Tui,
            ReminderAudience::Diagnostics,
        ]),
        body,
        summary,
        metadata,
    })
}
