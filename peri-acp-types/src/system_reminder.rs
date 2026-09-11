//! Canonical System Reminder V1 contract, policy projection, and bounded text codec.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SYSTEM_REMINDER_VERSION: u16 = 1;
pub const MAX_REMINDER_BODY_BYTES: usize = 64 * 1024;
pub const MAX_REMINDER_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_REMINDER_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_REMINDER_METADATA_DEPTH: usize = 16;
pub const MAX_REMINDER_METADATA_NODES: usize = 1_024;
pub const MAX_REMINDER_JSON_BYTES: usize = 96 * 1024;
pub const MAX_REMINDER_ATTRIBUTE_BYTES: usize = 24 * 1024;
pub const MAX_REMINDER_WIRE_BYTES: usize = 96 * 1024;
pub const MAX_REMINDER_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_REMINDERS_PER_MESSAGE: usize = 64;

const OPEN_PREFIX: &str = "<system-reminder";
const CLOSE: &str = "</system-reminder>";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderCategory {
    Capability,
    Task,
    Lifecycle,
    Guidance,
    Security,
    ExternalEvent,
    Diagnostic,
    Legacy,
}

/// Open producer namespace. Unknown source values remain round-trippable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReminderSource(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderDelivery {
    Required,
    Configurable,
    DiagnosticOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderAudience {
    Model,
    Tui,
    Diagnostics,
    Automation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReminderAudiences(pub Vec<ReminderAudience>);

impl ReminderAudiences {
    pub fn contains(&self, audience: ReminderAudience) -> bool {
        self.0.contains(&audience)
    }
}

/// Serializable protocol data only. Deserializing this type never establishes trust.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemReminder {
    pub version: u16,
    pub category: ReminderCategory,
    pub source: ReminderSource,
    pub kind: String,
    pub severity: ReminderSeverity,
    pub delivery: ReminderDelivery,
    pub audiences: ReminderAudiences,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default = "empty_metadata")]
    pub metadata: Value,
}

fn empty_metadata() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Deserialize)]
struct RawSystemReminder {
    version: u16,
    category: ReminderCategory,
    source: ReminderSource,
    kind: String,
    severity: ReminderSeverity,
    delivery: ReminderDelivery,
    audiences: ReminderAudiences,
    body: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default = "empty_metadata")]
    metadata: Value,
}

impl From<RawSystemReminder> for SystemReminder {
    fn from(raw: RawSystemReminder) -> Self {
        Self {
            version: raw.version,
            category: raw.category,
            source: raw.source,
            kind: raw.kind,
            severity: raw.severity,
            delivery: raw.delivery,
            audiences: raw.audiences,
            body: raw.body,
            summary: raw.summary,
            metadata: raw.metadata,
        }
    }
}

impl<'de> Deserialize<'de> for SystemReminder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RawSystemReminder::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReminderDecodeError {
    #[error("system reminder JSON exceeds {MAX_REMINDER_JSON_BYTES} bytes")]
    InputTooLarge,
    #[error("invalid system reminder JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] ReminderValidationError),
}

/// Decodes untrusted JSON through the bounded wire boundary into a validated, untrusted V1 DTO.
pub fn decode_system_reminder_json(input: &[u8]) -> Result<SystemReminder, ReminderDecodeError> {
    if input.len() > MAX_REMINDER_JSON_BYTES {
        return Err(ReminderDecodeError::InputTooLarge);
    }
    let raw: RawSystemReminder = serde_json::from_slice(input)?;
    let reminder = SystemReminder::from(raw);
    reminder.validate()?;
    Ok(reminder)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReminderValidationError {
    #[error("unsupported system reminder version {0}")]
    UnsupportedVersion(u16),
    #[error("trusted canonical producers cannot create legacy reminders")]
    LegacyProducerForbidden,
    #[error("{0} must be a non-empty snake_case identifier of at most 128 bytes")]
    InvalidIdentifier(&'static str),
    #[error("system reminder requires at least one unique audience")]
    InvalidAudiences,
    #[error("system reminder body exceeds {MAX_REMINDER_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("system reminder summary exceeds {MAX_REMINDER_SUMMARY_BYTES} bytes")]
    SummaryTooLarge,
    #[error("system reminder metadata exceeds {MAX_REMINDER_METADATA_BYTES} bytes")]
    MetadataTooLarge,
    #[error("system reminder metadata exceeds {MAX_REMINDER_METADATA_DEPTH} levels")]
    MetadataTooDeep,
    #[error("system reminder metadata exceeds {MAX_REMINDER_METADATA_NODES} nodes")]
    MetadataTooManyNodes,
    #[error("system reminder metadata must be an object")]
    InvalidMetadata,
}

impl SystemReminder {
    /// Validates schema facts only. This does not establish trusted provenance.
    pub fn validate(&self) -> Result<(), ReminderValidationError> {
        if self.version != SYSTEM_REMINDER_VERSION {
            return Err(ReminderValidationError::UnsupportedVersion(self.version));
        }
        if !valid_identifier(&self.source.0) {
            return Err(ReminderValidationError::InvalidIdentifier("source"));
        }
        if !valid_identifier(&self.kind) {
            return Err(ReminderValidationError::InvalidIdentifier("kind"));
        }
        if self.audiences.0.is_empty()
            || self
                .audiences
                .0
                .iter()
                .enumerate()
                .any(|(index, value)| self.audiences.0[..index].contains(value))
        {
            return Err(ReminderValidationError::InvalidAudiences);
        }
        if self.body.len() > MAX_REMINDER_BODY_BYTES {
            return Err(ReminderValidationError::BodyTooLarge);
        }
        if self
            .summary
            .as_ref()
            .is_some_and(|summary| summary.len() > MAX_REMINDER_SUMMARY_BYTES)
        {
            return Err(ReminderValidationError::SummaryTooLarge);
        }
        validate_metadata(&self.metadata)?;
        Ok(())
    }

    pub fn key(&self) -> ReminderKey {
        ReminderKey {
            source: self.source.clone(),
            kind: self.kind.clone(),
        }
    }
}

fn validate_metadata(metadata: &Value) -> Result<(), ReminderValidationError> {
    if !metadata.is_object() {
        return Err(ReminderValidationError::InvalidMetadata);
    }
    let mut bytes = 0usize;
    let mut nodes = 0usize;
    let mut stack = vec![(metadata, 1usize)];
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > MAX_REMINDER_METADATA_NODES {
            return Err(ReminderValidationError::MetadataTooManyNodes);
        }
        if depth > MAX_REMINDER_METADATA_DEPTH {
            return Err(ReminderValidationError::MetadataTooDeep);
        }
        match value {
            Value::Null => bytes = bytes.saturating_add(4),
            Value::Bool(value) => bytes = bytes.saturating_add(if *value { 4 } else { 5 }),
            Value::Number(value) => bytes = bytes.saturating_add(value.to_string().len()),
            Value::String(value) => bytes = bytes.saturating_add(value.len()),
            Value::Array(values) => {
                bytes = bytes.saturating_add(values.len());
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                bytes = bytes.saturating_add(values.len());
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
        }
        if bytes > MAX_REMINDER_METADATA_BYTES {
            return Err(ReminderValidationError::MetadataTooLarge);
        }
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && value.as_bytes()[0].is_ascii_lowercase()
}

/// Trusted provenance is deliberately non-serializable.
///
/// Only an explicit [`TrustedSystemReminderFactory`] at a trusted producer or recovery boundary can
/// construct this wrapper. Parsing JSON/XML yields [`SystemReminder`] or [`ParsedReminder`], never
/// this type.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustedSystemReminder {
    reminder: SystemReminder,
    provenance: TrustedReminderProvenance,
}

/// Auditable origins allowed to assert trusted system-reminder provenance.
///
/// This enum and [`TrustedSystemReminderFactory`] are intentionally not serde-enabled: wire data
/// cannot select a trusted origin or invoke the construction seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrustedReminderProvenance {
    Producer,
    Recovery,
}

/// Explicit capability used by trusted producer and recovery boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedSystemReminderFactory {
    provenance: TrustedReminderProvenance,
}

impl TrustedSystemReminderFactory {
    pub const fn for_producer() -> Self {
        Self {
            provenance: TrustedReminderProvenance::Producer,
        }
    }

    pub const fn for_recovery() -> Self {
        Self {
            provenance: TrustedReminderProvenance::Recovery,
        }
    }

    /// Validates canonical data and explicitly asserts the factory's trusted provenance.
    pub fn construct(
        self,
        reminder: SystemReminder,
    ) -> Result<TrustedSystemReminder, ReminderValidationError> {
        reminder.validate()?;
        if reminder.category == ReminderCategory::Legacy {
            return Err(ReminderValidationError::LegacyProducerForbidden);
        }
        Ok(TrustedSystemReminder {
            reminder,
            provenance: self.provenance,
        })
    }
}

impl TrustedSystemReminder {
    pub fn as_reminder(&self) -> &SystemReminder {
        &self.reminder
    }

    pub const fn provenance(&self) -> TrustedReminderProvenance {
        self.provenance
    }

    pub fn into_inner(self) -> SystemReminder {
        self.reminder
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReminderKey {
    pub source: ReminderSource,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderFilter {
    pub include_categories: Vec<ReminderCategory>,
    pub exclude_categories: Vec<ReminderCategory>,
    pub include_sources: Vec<ReminderSource>,
    pub exclude_sources: Vec<ReminderSource>,
    pub include_keys: Vec<ReminderKey>,
    pub exclude_keys: Vec<ReminderKey>,
    pub minimum_severity: Option<ReminderSeverity>,
    pub default_include: bool,
    pub allow_diagnostic_only: bool,
}

impl Default for ReminderFilter {
    fn default() -> Self {
        Self {
            include_categories: Vec::new(),
            exclude_categories: Vec::new(),
            include_sources: Vec::new(),
            exclude_sources: Vec::new(),
            include_keys: Vec::new(),
            exclude_keys: Vec::new(),
            minimum_severity: None,
            default_include: true,
            allow_diagnostic_only: false,
        }
    }
}

impl ReminderFilter {
    /// Applies the frozen precedence below to a reminder with explicit trusted provenance:
    /// audience > provenance/current-version validation > Required > DiagnosticOnly > kind >
    /// source > category > severity > default.
    ///
    /// At each filter dimension, exclusion wins over inclusion. A decision at a higher-precedence
    /// dimension is final; lower-precedence dimensions cannot override it.
    pub fn allows(&self, reminder: &TrustedSystemReminder, audience: ReminderAudience) -> bool {
        let reminder = reminder.as_reminder();
        if !reminder.audiences.contains(audience) {
            return false;
        }
        if reminder.validate().is_err() || reminder.category == ReminderCategory::Legacy {
            return false;
        }
        if reminder.delivery == ReminderDelivery::Required {
            return true;
        }
        if reminder.delivery == ReminderDelivery::DiagnosticOnly
            && (!self.allow_diagnostic_only || audience != ReminderAudience::Diagnostics)
        {
            return false;
        }

        let key = reminder.key();
        if let Some(decision) = filter_decision(
            self.exclude_keys.contains(&key),
            self.include_keys.contains(&key),
        ) {
            return decision;
        }
        if let Some(decision) = filter_decision(
            self.exclude_sources.contains(&reminder.source),
            self.include_sources.contains(&reminder.source),
        ) {
            return decision;
        }
        if let Some(decision) = filter_decision(
            self.exclude_categories.contains(&reminder.category),
            self.include_categories.contains(&reminder.category),
        ) {
            return decision;
        }
        if self
            .minimum_severity
            .is_some_and(|minimum| reminder.severity < minimum)
        {
            return false;
        }
        self.default_include
    }
}

fn filter_decision(excluded: bool, included: bool) -> Option<bool> {
    excluded
        .then_some(false)
        .or_else(|| included.then_some(true))
}

/// Diagnostic allowlist projection. Content and routing/control fields are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReminderDiagnostic {
    pub category: ReminderCategory,
    pub source: ReminderSource,
    pub kind: String,
    pub severity: ReminderSeverity,
}

impl From<&SystemReminder> for ReminderDiagnostic {
    fn from(reminder: &SystemReminder) -> Self {
        Self {
            category: reminder.category.clone(),
            source: reminder.source.clone(),
            kind: reminder.kind.clone(),
            severity: reminder.severity,
        }
    }
}

/// Parser provenance is intentionally not serializable and never denotes trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedReminderProvenance {
    LegacyText,
    UntrustedCanonicalText,
    OpaqueFutureVersion,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReminder {
    pub reminder: Option<SystemReminder>,
    pub raw: String,
    pub provenance: ParsedReminderProvenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReminderText {
    pub user_text: String,
    pub reminders: Vec<ParsedReminder>,
    pub resource_limit_reached: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReminderCodecError {
    #[error(transparent)]
    Validation(#[from] ReminderValidationError),
    #[error("encoded system reminder attributes exceed {MAX_REMINDER_ATTRIBUTE_BYTES} bytes")]
    AttributesTooLarge,
    #[error("encoded system reminder exceeds {MAX_REMINDER_WIRE_BYTES} bytes")]
    WireTooLarge,
    #[error("failed to serialize system reminder field: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Encodes legacy display/model context without asserting canonical producer provenance.
pub fn encode_legacy_system_reminder(body: &str) -> Result<String, ReminderCodecError> {
    if body.len() > MAX_REMINDER_BODY_BYTES {
        return Err(ReminderValidationError::BodyTooLarge.into());
    }
    let encoded = format!("<system-reminder>{}</system-reminder>", escape_xml(body));
    if encoded.len() > MAX_REMINDER_WIRE_BYTES {
        return Err(ReminderCodecError::WireTooLarge);
    }
    Ok(encoded)
}

/// Canonical wire encoder. Control-bearing output requires trusted provenance.
pub fn encode_system_reminder(
    reminder: &TrustedSystemReminder,
) -> Result<String, ReminderCodecError> {
    let reminder = reminder.as_reminder();
    reminder.validate()?;
    let audiences = serde_json::to_string(&reminder.audiences)?;
    let summary = serde_json::to_string(&reminder.summary)?;
    let metadata = serde_json::to_string(&reminder.metadata)?;
    let attributes = format!(
        " version=\"{}\" category=\"{}\" source=\"{}\" kind=\"{}\" severity=\"{}\" delivery=\"{}\" audiences=\"{}\" summary=\"{}\" metadata=\"{}\"",
        reminder.version,
        category_name(&reminder.category),
        escape_xml(&reminder.source.0),
        escape_xml(&reminder.kind),
        severity_name(reminder.severity),
        delivery_name(reminder.delivery),
        escape_xml(&audiences),
        escape_xml(&summary),
        escape_xml(&metadata),
    );
    if attributes.len() > MAX_REMINDER_ATTRIBUTE_BYTES {
        return Err(ReminderCodecError::AttributesTooLarge);
    }
    let encoded = format!(
        "{OPEN_PREFIX}{attributes}>{}{CLOSE}",
        escape_xml(&reminder.body)
    );
    if encoded.len() > MAX_REMINDER_WIRE_BYTES {
        return Err(ReminderCodecError::WireTooLarge);
    }
    Ok(encoded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderIngressMode {
    PreserveUntrusted,
    TrustedLegacy,
}

/// Parses reminder-looking text. Only `TrustedLegacy` removes recognized blocks from `user_text`.
pub fn parse_system_reminders_with_mode(
    text: &str,
    mode: ReminderIngressMode,
) -> ParsedReminderText {
    let mut output = ParsedReminderText {
        user_text: String::with_capacity(text.len().min(MAX_REMINDER_INPUT_BYTES)),
        reminders: Vec::new(),
        resource_limit_reached: false,
    };
    if text.len() > MAX_REMINDER_INPUT_BYTES {
        output.user_text.push_str(text);
        output.resource_limit_reached = true;
        return output;
    }

    let mut rest = text;
    while let Some(start) = rest.find(OPEN_PREFIX) {
        output.user_text.push_str(&rest[..start]);
        if output.reminders.len() == MAX_REMINDERS_PER_MESSAGE {
            output.user_text.push_str(&rest[start..]);
            output.resource_limit_reached = true;
            return output;
        }
        let candidate = &rest[start..];
        let Some(open_end) = candidate.find('>') else {
            output.user_text.push_str(candidate);
            return output;
        };
        if open_end > MAX_REMINDER_ATTRIBUTE_BYTES {
            output.user_text.push_str(candidate);
            output.resource_limit_reached = true;
            return output;
        }
        let header = &candidate[OPEN_PREFIX.len()..open_end];
        if !header.is_empty() && !header.starts_with(char::is_whitespace) {
            output.user_text.push_str(OPEN_PREFIX);
            rest = &candidate[OPEN_PREFIX.len()..];
            continue;
        }
        let after_open = &candidate[open_end + 1..];
        let Some(close_at) = after_open.find(CLOSE) else {
            output.user_text.push_str(candidate);
            return output;
        };
        if close_at > MAX_REMINDER_BODY_BYTES * 6
            || open_end + close_at + CLOSE.len() > MAX_REMINDER_WIRE_BYTES
        {
            output.user_text.push_str(candidate);
            output.resource_limit_reached = true;
            return output;
        }
        let raw = &candidate[..open_end + 1 + close_at + CLOSE.len()];
        let body = &after_open[..close_at];
        match parse_block(header, body, raw) {
            Some(parsed) => {
                if mode == ReminderIngressMode::PreserveUntrusted {
                    output.user_text.push_str(raw);
                }
                output.reminders.push(parsed);
            }
            None => output.user_text.push_str(raw),
        }
        rest = &after_open[close_at + CLOSE.len()..];
    }
    output.user_text.push_str(rest);
    output
}

/// Parses untrusted pasted text without deleting recognized canonical or future-version raw XML.
pub fn parse_system_reminders(text: &str) -> ParsedReminderText {
    parse_system_reminders_with_mode(text, ReminderIngressMode::PreserveUntrusted)
}

fn parse_block(header: &str, body: &str, raw: &str) -> Option<ParsedReminder> {
    let trimmed = header.trim();
    if trimmed.is_empty() {
        let body = unescape_xml(body)?;
        if body.len() > MAX_REMINDER_BODY_BYTES {
            return None;
        }
        return Some(ParsedReminder {
            reminder: Some(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Legacy,
                source: ReminderSource("legacy".into()),
                kind: "legacy_text".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
                body,
                summary: None,
                metadata: empty_metadata(),
            }),
            raw: raw.to_owned(),
            provenance: ParsedReminderProvenance::LegacyText,
        });
    }

    let attributes = parse_attributes(trimmed)?;
    let version = attribute(&attributes, "version")?.parse::<u16>().ok()?;
    if version != SYSTEM_REMINDER_VERSION {
        return Some(ParsedReminder {
            reminder: None,
            raw: raw.to_owned(),
            provenance: ParsedReminderProvenance::OpaqueFutureVersion,
        });
    }
    if attributes.len() != 9 {
        return None;
    }
    let reminder = SystemReminder {
        version,
        category: serde_json::from_value(Value::String(
            attribute(&attributes, "category")?.to_owned(),
        ))
        .ok()?,
        source: ReminderSource(attribute(&attributes, "source")?.to_owned()),
        kind: attribute(&attributes, "kind")?.to_owned(),
        severity: serde_json::from_value(Value::String(
            attribute(&attributes, "severity")?.to_owned(),
        ))
        .ok()?,
        delivery: serde_json::from_value(Value::String(
            attribute(&attributes, "delivery")?.to_owned(),
        ))
        .ok()?,
        audiences: serde_json::from_str(attribute(&attributes, "audiences")?).ok()?,
        body: unescape_xml(body)?,
        summary: serde_json::from_str(attribute(&attributes, "summary")?).ok()?,
        metadata: serde_json::from_str(attribute(&attributes, "metadata")?).ok()?,
    };
    reminder.validate().ok()?;
    Some(ParsedReminder {
        reminder: Some(reminder),
        raw: raw.to_owned(),
        provenance: ParsedReminderProvenance::UntrustedCanonicalText,
    })
}

fn parse_attributes(input: &str) -> Option<Vec<(String, String)>> {
    let mut rest = input;
    let mut attributes = Vec::new();
    while !rest.is_empty() {
        let name_end = rest.find('=')?;
        let name = &rest[..name_end];
        if !valid_identifier(name) || attributes.iter().any(|(existing, _)| existing == name) {
            return None;
        }
        rest = rest[name_end + 1..].strip_prefix('"')?;
        let value_end = rest.find('"')?;
        let value = unescape_xml(&rest[..value_end])?;
        attributes.push((name.to_owned(), value));
        rest = rest[value_end + 1..].trim_start();
    }
    Some(attributes)
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
}

fn category_name(category: &ReminderCategory) -> &'static str {
    match category {
        ReminderCategory::Capability => "capability",
        ReminderCategory::Task => "task",
        ReminderCategory::Lifecycle => "lifecycle",
        ReminderCategory::Guidance => "guidance",
        ReminderCategory::Security => "security",
        ReminderCategory::ExternalEvent => "external_event",
        ReminderCategory::Diagnostic => "diagnostic",
        ReminderCategory::Legacy => "legacy",
    }
}

fn severity_name(severity: ReminderSeverity) -> &'static str {
    match severity {
        ReminderSeverity::Info => "info",
        ReminderSeverity::Warning => "warning",
        ReminderSeverity::Error => "error",
        ReminderSeverity::Critical => "critical",
    }
}

fn delivery_name(delivery: ReminderDelivery) -> &'static str {
    match delivery {
        ReminderDelivery::Required => "required",
        ReminderDelivery::Configurable => "configurable",
        ReminderDelivery::DiagnosticOnly => "diagnostic_only",
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unescape_xml(value: &str) -> Option<String> {
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        result.push_str(&rest[..index]);
        let encoded = &rest[index..];
        let (decoded, consumed) = if encoded.starts_with("&amp;") {
            ('&', 5)
        } else if encoded.starts_with("&lt;") {
            ('<', 4)
        } else if encoded.starts_with("&gt;") {
            ('>', 4)
        } else if encoded.starts_with("&quot;") {
            ('"', 6)
        } else if encoded.starts_with("&apos;") {
            ('\'', 6)
        } else {
            return None;
        };
        result.push(decoded);
        rest = &encoded[consumed..];
    }
    result.push_str(rest);
    Some(result)
}

impl fmt::Display for ReminderSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "system_reminder_test.rs"]
mod tests;
