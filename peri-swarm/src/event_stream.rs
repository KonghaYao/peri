//! SSE frames for the actor→gateway hop (S5).
//!
//! The actor serves `GET /events` as `text/event-stream`; the gateway adapter
//! reads that stream and re-emits the same events through its own SSE response.
//! This module is the single definition of the hop's wire format: one encoder
//! used by the actor, one incremental parser used by the gateway.
//!
//! Only `item` frames carry an SSE `id` (the persisted `seq`); `turn`, the
//! official `agent.session.*` frames and `gap` are not persisted, matching the
//! public contract (`design §2.5`). Official frames travel under their own
//! event name (`event_type()`), so the gateway hop preserves them verbatim.
//!
//! A lagging subscription is told once (`gap`) and then closed by both SSE
//! writers: the dropped events may include a turn terminal, and silently
//! continuing would leave an official SDK waiting forever.

use serde_json::{json, Value};

use crate::domain::{ItemView, OfficialEvent, StreamEvent, TurnEvent};

pub const EVENT_ITEM: &str = "item";
pub const EVENT_TURN: &str = "turn";
pub const EVENT_GAP: &str = "gap";
/// SSE event-name prefix of the official Agents API event family.
pub const OFFICIAL_PREFIX: &str = "agent.session.";

/// One decoded SSE frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub event: String,
    pub id: Option<u64>,
    pub data: String,
}

impl Frame {
    /// Decode a frame into the domain event the gateway forwards. Unknown
    /// events (for example heartbeats) are ignored.
    #[must_use]
    pub fn into_stream_event(self) -> Option<StreamEvent> {
        match self.event.as_str() {
            EVENT_ITEM => serde_json::from_str::<ItemView>(&self.data)
                .ok()
                .map(StreamEvent::Item),
            EVENT_TURN => serde_json::from_str::<TurnEvent>(&self.data)
                .ok()
                .map(StreamEvent::Turn),
            EVENT_GAP => {
                let value: Value = serde_json::from_str(&self.data).ok()?;
                let from_seq = value.get("fromSeq")?.as_u64()?;
                let to_seq = value.get("toSeq")?.as_u64()?;
                Some(StreamEvent::Gap { from_seq, to_seq })
            }
            event if event.starts_with(OFFICIAL_PREFIX) => {
                serde_json::from_str::<OfficialEvent>(&self.data)
                    .ok()
                    .map(StreamEvent::Official)
            }
            _ => None,
        }
    }
}

/// Encodes domain events as SSE frames, dropping items at or below the cursor.
#[derive(Debug)]
pub struct FrameWriter {
    last_seq: u64,
}

impl FrameWriter {
    #[must_use]
    pub fn new(after: u64) -> Self {
        Self { last_seq: after }
    }

    /// `None` when the frame is a duplicate of an already delivered item.
    pub fn render(&mut self, event: &StreamEvent) -> Option<String> {
        match event {
            StreamEvent::Item(item) => {
                if item.seq <= self.last_seq {
                    return None;
                }
                self.last_seq = item.seq;
                Some(frame(
                    EVENT_ITEM,
                    Some(item.seq),
                    &serde_json::to_string(item).ok()?,
                ))
            }
            StreamEvent::Turn(turn) => {
                Some(frame(EVENT_TURN, None, &serde_json::to_string(turn).ok()?))
            }
            StreamEvent::Official(event) => Some(frame(
                event.event_type(),
                None,
                &serde_json::to_string(event).ok()?,
            )),
            StreamEvent::Gap { from_seq, to_seq } => Some(frame(
                EVENT_GAP,
                None,
                &json!({ "fromSeq": from_seq, "toSeq": to_seq }).to_string(),
            )),
        }
    }

    /// Frame for items dropped by a lagging broadcast subscription.
    #[must_use]
    pub fn gap(&self, skipped: u64) -> String {
        let from_seq = self.last_seq.saturating_add(1);
        let to_seq = self.last_seq.saturating_add(skipped);
        frame(
            EVENT_GAP,
            None,
            &json!({ "fromSeq": from_seq, "toSeq": to_seq }).to_string(),
        )
    }
}

fn frame(event: &str, id: Option<u64>, data: &str) -> String {
    let mut frame = String::new();
    frame.push_str("event: ");
    frame.push_str(event);
    frame.push('\n');
    if let Some(id) = id {
        frame.push_str("id: ");
        frame.push_str(&id.to_string());
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line);
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

/// Incremental `text/event-stream` parser.
///
/// Chunk boundaries are arbitrary: a frame can be split at any byte, so bytes
/// are buffered until the blank-line terminator arrives.
#[derive(Debug, Default)]
pub struct FrameParser {
    buffer: Vec<u8>,
}

impl FrameParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = find_terminator(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..end + 2).collect();
            if let Some(frame) = parse_block(&block[..end]) {
                frames.push(frame);
            }
        }
        frames
    }
}

/// Position of the blank-line terminator.
///
/// The actor encoder always terminates with `\n\n`; `\r` is stripped per line
/// so a CRLF server cannot leak `\r` into payloads.
fn find_terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}

fn parse_block(block: &[u8]) -> Option<Frame> {
    let text = std::str::from_utf8(block).ok()?;
    let mut event = None;
    let mut id = None;
    let mut data = String::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_owned()),
            "id" => id = value.parse::<u64>().ok(),
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
    }
    Some(Frame {
        event: event?,
        id,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ItemKind, ItemSource, OfficialEvent, OfficialSession, SessionStatus};

    fn item(seq: u64) -> ItemView {
        ItemView {
            id: format!("itim_{seq}"),
            seq,
            item_id: format!("itim_{seq}"),
            kind: ItemKind::Message,
            source: ItemSource::root(),
            payload: json!({ "text": format!("item-{seq}") }),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            truncated: false,
        }
    }

    #[test]
    fn encoder_and_parser_round_trip_every_event() {
        let mut writer = FrameWriter::new(0);
        let mut encoded = String::new();
        for event in [
            StreamEvent::Item(item(1)),
            StreamEvent::Turn(TurnEvent::started("req-1", "turn_1")),
            StreamEvent::Item(item(2)),
            StreamEvent::Turn(TurnEvent::completed("req-1", "turn_1", "end_turn")),
            StreamEvent::Gap {
                from_seq: 3,
                to_seq: 4,
            },
        ] {
            encoded.push_str(&writer.render(&event).expect("encode"));
        }

        // Split the byte stream at arbitrary points: parsing must be
        // boundary-independent.
        let bytes = encoded.as_bytes();
        let mut parser = FrameParser::new();
        let mut decoded = Vec::new();
        for chunk in bytes.chunks(3) {
            decoded.extend(parser.push(chunk));
        }
        let events: Vec<_> = decoded
            .into_iter()
            .map(|frame| frame.into_stream_event().expect("known event"))
            .collect();
        assert_eq!(events.len(), 5);
        assert_eq!(events[0], StreamEvent::Item(item(1)));
        assert_eq!(
            events[1],
            StreamEvent::Turn(TurnEvent::started("req-1", "turn_1"))
        );
        assert_eq!(
            events[3],
            StreamEvent::Turn(TurnEvent::completed("req-1", "turn_1", "end_turn"))
        );
        assert_eq!(
            events[4],
            StreamEvent::Gap {
                from_seq: 3,
                to_seq: 4
            }
        );
    }

    #[test]
    fn official_frames_keep_their_event_name_across_the_hop() {
        let mut writer = FrameWriter::new(0);
        let session = OfficialSession::new("agent-1", SessionStatus::Idle, 1, 2, None, None, None);
        let event = StreamEvent::Official(OfficialEvent::session_idle("turn_1", session));
        let encoded = writer.render(&event).expect("encode");
        assert!(
            encoded.starts_with("event: agent.session.idle\n"),
            "{encoded}"
        );
        let mut parser = FrameParser::new();
        let frame = parser.push(encoded.as_bytes()).pop().expect("frame");
        assert_eq!(frame.event, "agent.session.idle");
        assert_eq!(frame.into_stream_event(), Some(event));
    }

    #[test]
    fn items_at_or_below_the_cursor_are_not_replayed() {
        let mut writer = FrameWriter::new(2);
        assert!(writer.render(&StreamEvent::Item(item(2))).is_none());
        assert!(writer.render(&StreamEvent::Item(item(3))).is_some());
        assert_eq!(
            writer.gap(2),
            "event: gap\ndata: {\"fromSeq\":4,\"toSeq\":5}\n\n"
        );
    }

    #[test]
    fn non_event_frames_are_ignored() {
        let mut parser = FrameParser::new();
        let frames = parser.push(b"event: heartbeat\ndata: {\"lastItemSeq\":1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames
                .into_iter()
                .next()
                .expect("frame")
                .into_stream_event(),
            None
        );
    }
}
