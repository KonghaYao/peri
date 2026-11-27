//! Bounded validation of canonical tool text during History V1 audience checks.

use serde::Deserialize;
use std::io::{BufRead, BufReader, Cursor, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Head<'a> {
    version: u16,
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(borrow)]
    message: ToolHead<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolHead<'a> {
    role: &'a str,
    #[serde(rename = "id")]
    _id: &'a str,
    #[serde(default, rename = "tool_call_id")]
    _tool_call: Option<&'a str>,
    content: &'a str,
}

// The final two braces must close the message and the envelope, so marker-like
// text in a string or nested field cannot establish which string we are inside.
fn tool_content_start(prefix: &[u8]) -> Option<usize> {
    const MARKER: &[u8] = br#""content":""#;
    let prefix = &prefix[..prefix.len().min(4096)];
    let start = prefix.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    let mut head = prefix[..start].to_vec();
    head.extend_from_slice(br#""}}"#);
    let parsed: Head = serde_json::from_slice(&head).ok()?;
    (parsed.version == crate::store::PERSISTED_PAYLOAD_VERSION
        && parsed.kind == "message"
        && parsed.message.role == "tool"
        && parsed.message.content.is_empty())
    .then_some(start)
}

/// Validate large *ignored* tool text with the slice JSON parser in bounded
/// fragments. Only complete, independently valid JSON string contents can be
/// elided. The existing envelope/audience decoder sees all other bytes, including
/// trailing and duplicate fields. Unrecognized shapes use it unmodified.
pub fn history_body_displayable_to_client_buffered(
    reader: &mut dyn BufRead,
) -> Result<bool, serde_json::Error> {
    let prefix = reader.fill_buf().map_err(serde_json::Error::io)?;
    let Some(start) = tool_content_start(prefix) else {
        return super::history_body_displayable_to_client(BufReader::new(reader));
    };
    let head = prefix[..start].to_vec();
    reader.consume(start);
    let mut fragment = vec![b'"'];
    loop {
        let buf = reader.fill_buf().map_err(serde_json::Error::io)?;
        if buf.is_empty() {
            break;
        }
        let buf = &buf[..buf.len().min(64 * 1024)];
        fragment.extend_from_slice(buf);
        let count = buf.len();
        reader.consume(count);
        let end = fragment.len();
        fragment.push(b'"');
        let mut accepted = None;
        // A split can land inside UTF-8, a JSON escape, or a UTF-16 surrogate pair
        // (two six-byte escapes). Keep its incomplete tail for the next buffer.
        for keep in 0..=12.min(end.saturating_sub(2)) {
            let split = end - keep;
            let next = fragment[split];
            fragment[split] = b'"';
            let valid = serde_json::from_slice::<String>(&fragment[..=split]).is_ok();
            fragment[split] = next;
            if valid {
                accepted = Some(split);
                break;
            }
        }
        fragment.pop();
        // A closing quote, invalid escape or other structural byte stays intact
        // for the canonical decoder, along with the entire remaining stream.
        let Some(split) = accepted else { break };
        fragment.copy_within(split..end, 1);
        fragment.truncate(1 + end - split);
    }
    super::history_body_displayable_to_client(BufReader::new(
        Cursor::new(head)
            .chain(Cursor::new(&fragment[1..]))
            .chain(reader),
    ))
}

#[cfg(test)]
#[path = "buffered_tool_text_test.rs"]
mod tests;
