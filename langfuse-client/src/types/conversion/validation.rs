use sha2::{Digest, Sha256};

use super::{LangfuseError, OtelSpan};

pub(super) fn conversion_error(reason: &'static str) -> LangfuseError {
    LangfuseError::IngestionApi(format!("OTLP conversion: {reason}"))
}

pub(super) fn build_trace_id(id: Option<&str>) -> Result<String, LangfuseError> {
    map_id(id, 16, b"langfuse/otlp/trace\0", "missing trace identity")
}

pub(super) fn build_span_id(id: Option<&str>) -> Result<String, LangfuseError> {
    map_id(id, 8, b"langfuse/otlp/span\0", "missing span identity")
}

fn map_id(
    id: Option<&str>,
    byte_count: usize,
    domain: &[u8],
    missing_reason: &'static str,
) -> Result<String, LangfuseError> {
    let id = id
        .filter(|identity| !identity.trim().is_empty())
        .ok_or_else(|| conversion_error(missing_reason))?;
    let is_hex = id.bytes().all(|byte| byte.is_ascii_hexdigit());
    if id.len() == byte_count * 2 && is_hex {
        if id.bytes().all(|byte| byte == b'0') {
            return Err(conversion_error("zero identity is invalid"));
        }
        return Ok(id.to_ascii_lowercase());
    }

    let is_uuid = id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    let normalized =
        ((matches!(id.len(), 16 | 32) && is_hex) || is_uuid).then(|| id.to_ascii_lowercase());
    let id = normalized.as_deref().unwrap_or(id);

    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(id.as_bytes());
    let digest = hasher.finalize();
    let mut mapped = digest[..byte_count].to_vec();
    if mapped.iter().all(|byte| *byte == 0) {
        mapped[byte_count - 1] = 1;
    }
    let mut encoded = String::with_capacity(byte_count * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in mapped {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(encoded)
}

pub(super) fn build_span(
    trace_id: Option<&str>,
    span_id: Option<&str>,
    parent_id: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    timestamp: &str,
) -> Result<OtelSpan, LangfuseError> {
    let trace_id = build_trace_id(trace_id)?;
    let span_id = build_span_id(span_id)?;
    let parent_span_id = parent_id
        .map(|identity| build_span_id(Some(identity)))
        .transpose()?;
    let start = rfc3339_to_nano(start.unwrap_or(timestamp))
        .ok_or_else(|| conversion_error("invalid start time"))?;
    let end = match end {
        Some(end) => rfc3339_to_nano(end).ok_or_else(|| conversion_error("invalid end time"))?,
        None => start,
    };
    if end < start {
        return Err(conversion_error("end time precedes start time"));
    }
    Ok(OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: None,
        kind: Some(1),
        start_time_unix_nano: Some(start.to_string()),
        end_time_unix_nano: Some(end.to_string()),
        attributes: None,
        status: None,
    })
}

fn rfc3339_to_nano(timestamp: &str) -> Option<u64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let seconds = u64::try_from(parsed.timestamp()).ok()?;
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(u64::from(parsed.timestamp_subsec_nanos()))
}
