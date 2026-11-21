//! Monitor WebSocket.
//!
//! On connect the viewer receives `hello`, the current `tabs` snapshot and the
//! banner of a handoff that is already pending, then state snapshots from
//! [`crate::session::Session`] and the JPEG stream from the read-only CDP
//! supervisor. Every binary frame is preceded by a `meta` message, so the client
//! always knows the viewport before it paints.
//!
//! Input follows ownership (README §6.2): while the MCP holds the page, the
//! viewer is a read-only CDP viewer, so mouse, wheel, keyboard and text frames
//! are rejected here and never queued for a later takeover. In `HumanHolds`
//! they are mapped to CSS pixels and forwarded to the session's controller;
//! navigation and history go to the controller too, and only fall back to the
//! MCP as `command` messages while the agent is the writer.
//!
//! Frames stay on the viewer connection: ownership never moves the screencast to
//! the controller (README §6.4).

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures::{Sink, SinkExt, StreamExt};
use tokio::sync::broadcast;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::cdp::Viewport;
use crate::http::AppState;
use crate::ownership::{ControllerCommand, OwnershipState, Snapshot};
use crate::protocol::{
    BrowserCommand, ClientMessage, GatewayMessage, MouseEventKind, ServerMessage,
};
use crate::session::Session;
use crate::stream::{Frame, FrameMeta};

/// Serves one monitor viewer until it disconnects.
pub async fn handle(socket: WebSocket, session: Session, state: AppState) {
    let session_id = session.id.clone();
    let sessions = state.sessions.clone();
    sessions.viewer_joined(&session_id).await;
    serve_viewer(socket, session, state).await;
    sessions.viewer_left(&session_id).await;
}

async fn serve_viewer(socket: WebSocket, session: Session, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let mut events = session.subscribe();
    let mut frames = session.subscribe_frames();
    debug!(
        session = %session.id,
        receivers = frames.len(),
        "viewer subscribed to frames"
    );
    let mut visible = true;
    let mut latest: Option<Arc<Frame>> = None;
    // Page this viewer is showing. The screencast supervisor only streams the
    // session's active page, so a frame for anything else is a leftover of a
    // tab switch that this socket has already moved past.
    let mut active: Option<String> = session.effective_active().map(str::to_owned);

    if !send_json(&mut sink, &session.hello_message()).await
        || !send_json(&mut sink, &session.tabs_message()).await
    {
        return;
    }
    // A viewer that connects while the MCP is waiting for a human must see the
    // banner immediately; the handoff is otherwise only broadcast on change.
    if let Some(handoff) = session.handoff_message() {
        if !send_json(&mut sink, &handoff).await {
            return;
        }
    }
    // Reconnect paint: a static page produces no new damage, so replay the
    // cached frame of the tab this session has active.
    if let Some(frame) = active
        .as_deref()
        .and_then(|page_id| session.cached_frame(page_id))
    {
        let _ = send_frame(&mut sink, &frame).await;
    }
    info!(session = %session.id, "viewer connected");

    loop {
        tokio::select! {
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let action = handle_text(&mut sink, &session, &state, text.as_str()).await;
                        if let Some(wants_frames) = action.visible {
                            visible = wants_frames;
                            if visible {
                                // Replay the newest frame after a hidden period:
                                // a static page produces no new damage to stream.
                                if let Some(frame) = latest.as_ref() {
                                    if !send_frame(&mut sink, frame).await {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(page_id) = action.switched {
                            active = Some(page_id);
                            // The tab we just entered may have no damage to
                            // stream yet. Paint its last known frame instead of
                            // leaving the canvas on the previous tab.
                            if let Some(frame) = active
                                .as_deref()
                                .and_then(|page_id| session.cached_frame(page_id))
                            {
                                latest = Some(frame.clone());
                                if visible && !send_frame(&mut sink, &frame).await {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        warn!(session = %session.id, "ignoring binary client frame");
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    // Ping/Pong are answered by the WebSocket layer.
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        debug!(session = %session.id, error = %error, "viewer socket error");
                        break;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(message) => {
                        // The active page is session-wide: another viewer's
                        // switch moves this strip too, so this socket must also
                        // move its canvas to the page the strip now points at.
                        let announced = match &message {
                            ServerMessage::Tabs { tabs } => Some(
                                tabs.iter()
                                    .find(|tab| tab.active)
                                    .map(|tab| tab.id.clone()),
                            ),
                            _ => None,
                        };
                        if !send_json(&mut sink, &message).await {
                            break;
                        }
                        if let Some(next) = announced {
                            if next != active {
                                active = next;
                                latest = None;
                                if let Some(frame) = active
                                    .as_deref()
                                    .and_then(|page_id| session.cached_frame(page_id))
                                {
                                    latest = Some(frame.clone());
                                    if visible && !send_frame(&mut sink, &frame).await {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(
                            session = %session.id,
                            skipped,
                            "viewer lagged; resynchronising from the next snapshot"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            frame = frames.recv() => {
                match frame {
                    Ok(frame) => {
                        if active.as_deref() == Some(frame.meta.page_id.as_str()) {
                            latest = Some(frame.clone());
                            if visible && !send_frame(&mut sink, &frame).await {
                                break;
                            }
                        } else {
                            debug!(
                                session = %session.id,
                                page = %frame.meta.page_id,
                                "dropping a frame for a page this viewer left"
                            );
                        }
                    }
                    // Capacity one: a lagging viewer skipped to the newest
                    // frame; the next receive returns it.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {}
                }
            }
        }
    }
    info!(session = %session.id, "viewer disconnected");
}

#[derive(Debug, Default)]
struct TextAction {
    /// Client asked to stop or resume frames.
    visible: Option<bool>,
    /// Page the session accepted for `switch`. The caller replays its cached
    /// frame, because a page that has just become active may not have produced
    /// any damage for the screencast yet.
    switched: Option<String>,
}

/// Handles one text frame from the viewer.
async fn handle_text<S>(sink: &mut S, session: &Session, state: &AppState, text: &str) -> TextAction
where
    S: Sink<Message> + Unpin,
{
    let message = match serde_json::from_str::<ClientMessage>(text) {
        Ok(message) => message,
        Err(error) => {
            warn!(session = %session.id, error = %error, "ignoring malformed viewer message");
            return TextAction::default();
        }
    };
    let kind = message.kind();
    // One read of the live ownership state per frame. Input is accepted only in
    // `HumanHolds` with an open controller; a viewer that raced the handoff gets
    // a `read_only` error instead of an input that would be queued and replayed
    // later (README §6.3).
    let snapshot = session.ownership.snapshot();
    let interactive = snapshot.interactive();

    match message {
        ClientMessage::Ack => trace!(session = %session.id, "frame acknowledged"),
        ClientMessage::Visible { visible } => {
            debug!(session = %session.id, visible, "viewer visibility changed");
            return TextAction {
                visible: Some(visible),
                switched: None,
            };
        }
        ClientMessage::Switch { page_id } => {
            // The store broadcasts the new tab strip to every viewer: the
            // active page is session-wide, so other viewers must not keep a
            // strip that disagrees with the frames they receive.
            if state
                .sessions
                .set_active_page(&session.id, &page_id)
                .await
                .is_none()
            {
                warn!(
                    session = %session.id,
                    page = %page_id,
                    "ignoring switch to a page this session does not own"
                );
            } else {
                return TextAction {
                    switched: Some(page_id),
                    ..TextAction::default()
                };
            }
        }
        ClientMessage::Takeover => {
            if snapshot.page_id.is_none() {
                warn!(session = %session.id, "ignoring takeover: session has no pages");
                send_error(sink, "unavailable", "this session has no page to take over").await;
            } else {
                info!(session = %session.id, "takeover requested by a viewer");
                session.ownership.takeover();
            }
        }
        ClientMessage::Release => {
            if snapshot.state == OwnershipState::HumanHolds {
                info!(session = %session.id, "the viewer released the page");
                session.ownership.release();
            } else {
                debug!(session = %session.id, kind, "release without a controller");
                send_error(
                    sink,
                    "read_only",
                    "the agent holds this page; there is nothing to release",
                )
                .await;
            }
        }
        ClientMessage::Answer {
            request_id,
            accepted,
        } => {
            let pending = snapshot
                .handoff
                .as_ref()
                .map(|handoff| handoff.request_id.as_str());
            if pending == Some(request_id.as_str()) {
                debug!(
                    session = %session.id,
                    request_id = %request_id,
                    accepted,
                    "handoff answered"
                );
                session.ownership.answer(request_id, accepted);
            } else {
                warn!(
                    session = %session.id,
                    request_id = %request_id,
                    "ignoring an answer without a matching handoff"
                );
                send_error(sink, "no_handoff", "no handoff is waiting for an answer").await;
            }
        }
        ClientMessage::Mouse {
            event,
            x,
            y,
            button,
            click_count,
        } => {
            if interactive {
                let (x, y) = pointer_css(&snapshot, session, x, y);
                let click_count = click_count.unwrap_or(match event {
                    MouseEventKind::Move => 0,
                    MouseEventKind::Down | MouseEventKind::Up => 1,
                });
                session.ownership.input(ControllerCommand::Mouse {
                    event,
                    x,
                    y,
                    button,
                    click_count,
                    // The wire `mouse` frame carries no modifier bits; keyboard
                    // shortcuts carry them (`key.modifiers`).
                    modifiers: 0,
                });
            } else {
                reject_input(sink, session, kind).await;
            }
        }
        ClientMessage::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            if interactive {
                let (x, y) = pointer_css(&snapshot, session, x, y);
                session.ownership.input(ControllerCommand::Wheel {
                    x,
                    y,
                    delta_x,
                    delta_y,
                });
            } else {
                reject_input(sink, session, kind).await;
            }
        }
        ClientMessage::Key {
            event,
            key,
            code,
            text,
            modifiers,
        } => {
            if interactive {
                session.ownership.input(ControllerCommand::Key {
                    event,
                    key,
                    code,
                    text,
                    modifiers,
                });
            } else {
                reject_input(sink, session, kind).await;
            }
        }
        ClientMessage::Text { text } => {
            if interactive {
                session.ownership.input(ControllerCommand::Text(text));
            } else {
                reject_input(sink, session, kind).await;
            }
        }
        ClientMessage::Navigate { url } => {
            if interactive {
                session.ownership.input(ControllerCommand::Navigate(url));
            } else {
                forward_command(sink, session, state, BrowserCommand::Navigate { url }).await;
            }
        }
        ClientMessage::History { delta } => {
            if interactive {
                session.ownership.input(ControllerCommand::History(delta));
            } else {
                forward_command(sink, session, state, BrowserCommand::History { delta }).await;
            }
        }
        ClientMessage::Devtools => match session.devtools_url() {
            Some((url, page_id)) => {
                // Kitesurf accepts one controller per page. Handing the page to
                // the official frontend means the gateway stops writing first.
                session.ownership.release_for_devtools();
                info!(session = %session.id, %page_id, "page handed to Chrome DevTools");
                send_json(sink, &ServerMessage::Devtools { url, page_id }).await;
            }
            None => {
                send_error(sink, "no_page", "this session has no page to debug").await;
            }
        },
        ClientMessage::NewTab { url } => {
            forward_command(sink, session, state, BrowserCommand::NewTab { url }).await;
        }
        ClientMessage::CloseTab { page_id } => {
            forward_command(
                sink,
                session,
                state,
                BrowserCommand::CloseTab {
                    page_id: page_id.clone(),
                },
            )
            .await;
        }
        ClientMessage::Unknown => {
            warn!(session = %session.id, "ignoring unknown viewer message");
        }
    }
    TextAction::default()
}

/// Maps a pointer reported in canvas pixels to CSS pixels of the page viewport.
///
/// The monitor sends the position in the JPEG's pixel space (its canvas may be
/// CSS-scaled, so canvas pixels are not CSS pixels). The last screencast frame
/// describes the captured box: `deviceWidth × deviceHeight` is the image size
/// and `viewportWidth × viewportHeight` the CSS box it captured, so
///
/// ```text
/// css_x = canvas_x × css_viewport_width  / device_width
/// css_y = canvas_y × css_viewport_height / device_height
/// ```
///
/// The ratio folds in `pageScaleFactor` (Chromium reports `deviceWidth =
/// viewportWidth × pageScaleFactor`). `layout` is `Page.getLayoutMetrics`'s live
/// `cssLayoutViewport`: it wins when present, because the window may have been
/// resized between the last frame and this click, and a stale ratio would aim at
/// the wrong element. The result is clamped to the viewport, so a stale canvas
/// cannot produce off-page coordinates (README §6.5).
#[must_use]
pub fn canvas_to_css(
    x: f64,
    y: f64,
    meta: Option<&FrameMeta>,
    layout: Option<Viewport>,
) -> (f64, f64) {
    let Some(meta) = meta else {
        return (x, y);
    };
    if meta.device_width == 0 || meta.device_height == 0 {
        return (x, y);
    }
    // A zero-sized viewport carries no information (a page that is not laid
    // out yet); fall back to the frame's own metadata instead of collapsing
    // every click to the origin.
    let layout = layout.filter(|viewport| viewport.width > 0 && viewport.height > 0);
    let width = f64::from(layout.map_or(meta.viewport_width, |viewport| viewport.width));
    let height = f64::from(layout.map_or(meta.viewport_height, |viewport| viewport.height));
    let scale_x = width / f64::from(meta.device_width);
    let scale_y = height / f64::from(meta.device_height);
    (
        (x * scale_x).clamp(0.0, (width - 1.0).max(0.0)),
        (y * scale_y).clamp(0.0, (height - 1.0).max(0.0)),
    )
}

/// Pointer position in CSS pixels for the page the human currently writes.
fn pointer_css(snapshot: &Snapshot, session: &Session, x: f64, y: f64) -> (f64, f64) {
    let frame = snapshot
        .page_id
        .as_deref()
        .and_then(|page_id| session.cached_frame(page_id));
    let meta = frame.as_deref().map(|frame| &frame.meta);
    canvas_to_css(x, y, meta, snapshot.viewport)
}

/// Rejects page input while the agent is the writer.
async fn reject_input<S>(sink: &mut S, session: &Session, kind: &str)
where
    S: Sink<Message> + Unpin,
{
    debug!(session = %session.id, kind, "rejecting page input while read-only");
    send_error(
        sink,
        "read_only",
        "the agent holds this page; the monitor is read-only",
    )
    .await;
}

/// Sends a page command to the MCP, which owns the page connection.
async fn forward_command<S>(
    sink: &mut S,
    session: &Session,
    state: &AppState,
    command: BrowserCommand,
) where
    S: Sink<Message> + Unpin,
{
    // Read the live session: the snapshot taken at connect time may predate a
    // tab switch.
    let current = state
        .sessions
        .get(&session.id)
        .await
        .unwrap_or_else(|| session.clone());
    let Some(page_id) = current.effective_active().map(str::to_owned) else {
        send_error(sink, "unavailable", "this session has no page to act on").await;
        return;
    };

    let message = GatewayMessage::Command {
        session_id: current.id.clone(),
        request_id: Uuid::new_v4().to_string(),
        page_id,
        command,
    };
    if let Err(error) = state.registry.send(&current.id, message).await {
        warn!(
            session = %current.id,
            error = %error,
            "page command could not be delivered"
        );
        send_error(
            sink,
            "unavailable",
            "no agent is connected to execute this command",
        )
        .await;
    }
}

async fn send_error<S>(sink: &mut S, code: &str, message: &str)
where
    S: Sink<Message> + Unpin,
{
    let _ = send_json(
        sink,
        &ServerMessage::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        },
    )
    .await;
}

/// Sends one JPEG frame together with the metadata that must precede it.
async fn send_frame<S>(sink: &mut S, frame: &Frame) -> bool
where
    S: Sink<Message> + Unpin,
{
    send_json(sink, &frame.meta_message()).await
        && sink.send(Message::Binary(frame.jpeg.clone())).await.is_ok()
}

async fn send_json<S>(sink: &mut S, message: &ServerMessage) -> bool
where
    S: Sink<Message> + Unpin,
{
    let text = match serde_json::to_string(message) {
        Ok(text) => text,
        Err(error) => {
            tracing::error!(error = %error, "cannot encode server message");
            return false;
        }
    };
    sink.send(Message::Text(text.into())).await.is_ok()
}

#[cfg(test)]
#[path = "ws_test.rs"]
mod tests;
