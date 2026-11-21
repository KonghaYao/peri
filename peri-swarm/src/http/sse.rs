use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures::Stream;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::time::{interval, Interval, MissedTickBehavior};

use crate::domain::{CreateSessionResponse, StreamEvent};
use crate::service::{CreateSessionResult, EventSubscription};

/// Successful `POST /sessions?stream=true` response: one `session` event, then
/// the first turn's live events. The subscription was opened by the handler
/// before the service executed the turn.
pub fn create_response(
    result: &CreateSessionResult,
    subscription: EventSubscription,
    heartbeat: Duration,
) -> Response {
    let session = CreateSessionResponse::new(result.session.clone(), result.created);
    let initial = vec![json_event("session", &session)];
    event_response_with_initial(subscription, heartbeat, initial)
}

pub fn event_response(subscription: EventSubscription, heartbeat: Duration) -> Response {
    event_response_with_initial(subscription, heartbeat, Vec::new())
}

fn event_response_with_initial(
    subscription: EventSubscription,
    heartbeat: Duration,
    initial: Vec<Event>,
) -> Response {
    let stream = event_stream(subscription, heartbeat, initial);
    let mut response = Sse::new(stream).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// Persisted backlog, then live broadcasts, with a 15 s heartbeat by default.
///
/// Only `item` events carry an SSE `id` equal to `seq`; `turn`, the official
/// `agent.session.*` frames and `gap` are not persisted in `swarm_items` and
/// therefore do not advance `Last-Event-ID`. Official frames are additive: the
/// gateway's own `item`/`turn` events stay on the stream.
///
/// The heartbeat payload's `lastItemSeq` is the session's current
/// `MAX(seq)` (read from the shared tail), not the last seq this stream
/// delivered. It reports progress; a client resumes from an item's `seq`.
pub fn event_stream(
    subscription: EventSubscription,
    heartbeat: Duration,
    initial: Vec<Event>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let state = StreamState::new(subscription, heartbeat, initial);
    futures::stream::unfold(Some(state), |slot| async move {
        let mut state = slot?;
        loop {
            if let Some(event) = state.next_ready() {
                return Some((Ok(event), Some(state)));
            }
            tokio::select! {
                received = state.live.recv() => {
                    match received {
                        Ok(event) => {
                            if let Some(event) = state.render(event) {
                                return Some((Ok(event), Some(state)));
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Some((Ok(state.gap(skipped)), Some(state)));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
                _ = state.heartbeat.tick() => {
                    let event = Event::default()
                        .event("heartbeat")
                        .data(
                            json!({ "lastItemSeq": state.session_tail.load(Ordering::Relaxed) })
                                .to_string(),
                        );
                    return Some((Ok(event), Some(state)));
                }
            }
        }
    })
}

struct StreamState {
    initial: VecDeque<Event>,
    backlog: VecDeque<StreamEvent>,
    live: broadcast::Receiver<StreamEvent>,
    heartbeat: Interval,
    /// Last seq delivered on this stream (dedupe and gap bookkeeping).
    last_seq: u64,
    /// Session tail shared with the item log, used by heartbeats.
    session_tail: Arc<AtomicU64>,
}

impl StreamState {
    fn new(subscription: EventSubscription, heartbeat: Duration, initial: Vec<Event>) -> Self {
        let session_tail = subscription.tail();
        let (backlog, live) = subscription.into_parts();
        let mut heartbeat = interval(heartbeat.max(Duration::from_millis(1)));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        heartbeat.reset();
        Self {
            initial: initial.into(),
            backlog: backlog.into(),
            live,
            heartbeat,
            last_seq: 0,
            session_tail,
        }
    }

    fn next_ready(&mut self) -> Option<Event> {
        if let Some(event) = self.initial.pop_front() {
            return Some(event);
        }
        while let Some(event) = self.backlog.pop_front() {
            if let Some(rendered) = self.render(event) {
                return Some(rendered);
            }
        }
        None
    }

    fn render(&mut self, event: StreamEvent) -> Option<Event> {
        match event {
            StreamEvent::Item(item) => {
                // Backlog and the live receiver can overlap; seq is the dedupe key.
                if item.seq <= self.last_seq {
                    return None;
                }
                self.last_seq = item.seq;
                Some(json_event("item", &item).id(item.seq.to_string()))
            }
            StreamEvent::Turn(turn) => Some(json_event("turn", &turn)),
            StreamEvent::Official(event) => Some(json_event(event.event_type(), &event)),
            StreamEvent::Gap { from_seq, to_seq } => Some(json_event(
                "gap",
                &json!({ "fromSeq": from_seq, "toSeq": to_seq }),
            )),
        }
    }

    fn gap(&self, skipped: u64) -> Event {
        json_event(
            "gap",
            &json!({
                "fromSeq": self.last_seq.saturating_add(1),
                "toSeq": self.last_seq.saturating_add(skipped),
            }),
        )
    }
}

fn json_event<T: serde::Serialize>(name: &'static str, data: &T) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(data).unwrap_or_else(|_| "{}".to_owned()))
}
