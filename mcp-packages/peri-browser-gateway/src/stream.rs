//! Read-only page streaming for monitor viewers.
//!
//! One screencast per session page, not one per viewer (README §6.4): the first
//! viewer starts a viewer-role CDP connection and `Page.startScreencast`; every
//! connected monitor receives the same JPEG through a `broadcast` channel. The
//! channel holds a single slot, so a slow viewer loses intermediate frames and
//! always converges on the newest one.
//!
//! Ownership is unchanged by this module: the connection is always
//! `?role=viewer`, so Kitesurf rejects input and navigation. Tab thumbnails use
//! `Page.captureScreenshot` on short-lived viewer connections.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant};
use tracing::{debug, info, warn};

use crate::cdp::{self, CdpError, Viewer};
use crate::protocol::{PageRef, ServerMessage};
use crate::session::Session;

/// Frames are a "keep the newest" stream: capacity one makes every lagging
/// receiver skip straight to the latest frame.
pub const FRAME_CAPACITY: usize = 1;
/// Thumbnail captures are stills; a slow page must not hold the tab strip.
pub const THUMBNAIL_TIMEOUT: Duration = Duration::from_millis(800);
/// A different tab invalidates the previous active stream immediately; a failed
/// connection retries on this cadence instead of spinning.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Supervisor wake-up used to notice finished child tasks.
pub const TICK: Duration = Duration::from_millis(250);

/// One JPEG frame plus the metadata that must precede it on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub jpeg: Bytes,
    pub meta: FrameMeta,
}

impl Frame {
    /// Metadata message sent immediately before the binary frame.
    #[must_use]
    pub fn meta_message(&self) -> ServerMessage {
        ServerMessage::Meta {
            page_id: self.meta.page_id.clone(),
            viewport_width: self.meta.viewport_width,
            viewport_height: self.meta.viewport_height,
            device_width: self.meta.device_width,
            device_height: self.meta.device_height,
            page_scale_factor: self.meta.page_scale_factor,
            scroll_offset_x: self.meta.scroll_offset_x,
            scroll_offset_y: self.meta.scroll_offset_y,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameMeta {
    pub page_id: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub device_width: u32,
    pub device_height: u32,
    pub page_scale_factor: f64,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
}

/// Handle to one session's supervisor task.
///
/// Dropping the handle stops the task: the command channel closes and the
/// supervisor aborts its page and thumbnail tasks.
#[derive(Debug)]
pub struct StreamHandle {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
}

impl StreamHandle {
    pub fn viewer_joined(&self) {
        let _ = self.commands.send(Command::ViewerJoined);
    }

    pub fn viewer_left(&self) {
        let _ = self.commands.send(Command::ViewerLeft);
    }

    pub fn pages(&self, pages: Vec<PageRef>) {
        let _ = self.commands.send(Command::Pages(pages));
    }

    pub fn active(&self, page_id: Option<String>) {
        let _ = self.commands.send(Command::Active(page_id));
    }

    pub fn cdp_url(&self, url: String) {
        let _ = self.commands.send(Command::CdpUrl(url));
    }
}

#[derive(Debug)]
enum Command {
    ViewerJoined,
    ViewerLeft,
    Pages(Vec<PageRef>),
    Active(Option<String>),
    CdpUrl(String),
}

struct State {
    cdp_url: String,
    pages: Vec<PageRef>,
    active: Option<String>,
    viewers: usize,
    thumbs_dirty: bool,
}

/// Starts the supervisor for one session.
///
/// The supervisor connects only after the first viewer arrives and drops the
/// page connection when the last one leaves, so a monitor page nobody has open
/// costs no browser work.
#[must_use]
pub fn spawn(session: &Session) -> StreamHandle {
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = State {
        cdp_url: session.cdp_ws.clone(),
        pages: session.pages.clone(),
        active: session.active_page.clone(),
        viewers: 0,
        thumbs_dirty: true,
    };
    let frames = session.frame_sender();
    let messages = session.message_sender();
    let cache = session.frame_cache();
    let session_id = session.id.clone();
    tokio::spawn(run(session_id, receiver, state, frames, messages, cache));
    StreamHandle { commands }
}

async fn run(
    session_id: String,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    mut state: State,
    frames: broadcast::Sender<Arc<Frame>>,
    messages: broadcast::Sender<ServerMessage>,
    cache: Arc<Mutex<HashMap<String, Arc<Frame>>>>,
) {
    let mut page_task: Option<JoinHandle<Result<(), CdpError>>> = None;
    let mut current_page: Option<String> = None;
    let mut thumb_task: Option<JoinHandle<()>> = None;
    let mut retry_at = Instant::now();
    let mut reported_error: Option<String> = None;

    loop {
        if page_task.as_ref().is_some_and(JoinHandle::is_finished) {
            let task = page_task.take().expect("checked above");
            match task.now_or_never() {
                Some(Ok(Ok(()))) => debug!(session = %session_id, "page stream ended"),
                Some(Ok(Err(error))) => {
                    let message = error.to_string();
                    if reported_error.as_deref() != Some(message.as_str()) {
                        warn!(session = %session_id, error = %message, "page stream failed");
                        let _ = messages.send(ServerMessage::Error {
                            code: "cdp_unavailable".to_owned(),
                            message: format!("cannot stream the active page: {message}"),
                        });
                        reported_error = Some(message);
                    }
                }
                Some(Err(error)) => {
                    debug!(session = %session_id, error = %error, "page task stopped")
                }
                None => {}
            }
            current_page = None;
            retry_at = Instant::now() + RECONNECT_DELAY;
        }

        if thumb_task.as_ref().is_some_and(JoinHandle::is_finished) {
            thumb_task = None;
        }

        let desired = desired_page(&state);
        if desired != current_page {
            if let Some(task) = page_task.take() {
                task.abort();
            }
            current_page = None;
            // The cache is keyed by page, so the frame of the page we just left
            // stays available for switching back. Viewers replay the newly
            // active page's frame themselves; the supervisor cannot order a
            // replay against the `tabs` broadcast.
            // An explicit tab/page change is not a failure retry: connect now.
            retry_at = Instant::now();
        }

        if state.viewers == 0 {
            if let Some(task) = page_task.take() {
                task.abort();
            }
            current_page = None;
            if let Some(task) = thumb_task.take() {
                task.abort();
            }
            state.thumbs_dirty = true;
        } else if page_task.is_none() && desired.is_some() && Instant::now() >= retry_at {
            let target = desired.expect("checked above");
            let page = state
                .pages
                .iter()
                .find(|page| page.target_id == target)
                .cloned()
                .expect("desired page comes from the page list");
            page_task = Some(tokio::spawn(stream_page(
                session_id.clone(),
                state.cdp_url.clone(),
                page,
                frames.clone(),
                cache.clone(),
            )));
            current_page = Some(target);
            retry_at = Instant::now() + RECONNECT_DELAY;
        }

        if state.viewers > 0 && thumb_task.is_none() && state.thumbs_dirty && state.pages.len() > 1
        {
            state.thumbs_dirty = false;
            thumb_task = Some(tokio::spawn(refresh_thumbs(
                state.cdp_url.clone(),
                state.pages.clone(),
                desired_page(&state),
                messages.clone(),
            )));
        }

        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(Command::ViewerJoined) => {
                        if state.viewers == 0 {
                            retry_at = Instant::now();
                        }
                        state.viewers = state.viewers.saturating_add(1);
                        state.thumbs_dirty = true;
                    }
                    Some(Command::ViewerLeft) => {
                        state.viewers = state.viewers.saturating_sub(1);
                        state.thumbs_dirty = true;
                    }
                    Some(Command::Pages(pages)) => {
                        // A closed target's frame is unreachable: drop it so the
                        // cache stays bounded by the tab count.
                        cache
                            .lock()
                            .expect("frame cache poisoned")
                            .retain(|page_id, _| {
                                pages.iter().any(|page| &page.target_id == page_id)
                            });
                        state.pages = pages;
                        state.thumbs_dirty = true;
                    }
                    Some(Command::Active(active)) => {
                        state.active = active;
                        state.thumbs_dirty = true;
                    }
                    Some(Command::CdpUrl(url)) => {
                        if state.cdp_url != url {
                            state.cdp_url = url;
                            if let Some(task) = page_task.take() {
                                task.abort();
                            }
                            current_page = None;
                            // Frames came from the previous browser: they can no
                            // longer match the targets this session now owns.
                            cache.lock().expect("frame cache poisoned").clear();
                            retry_at = Instant::now();
                        }
                    }
                    None => break,
                }
            }
            () = sleep(TICK) => {}
        }
    }

    if let Some(task) = page_task {
        task.abort();
    }
    if let Some(task) = thumb_task {
        task.abort();
    }
    info!(session = %session_id, "stream supervisor stopped");
}

fn desired_page(state: &State) -> Option<String> {
    if state.viewers == 0 {
        return None;
    }
    if let Some(active) = state.active.as_ref() {
        if state.pages.iter().any(|page| &page.target_id == active) {
            return Some(active.clone());
        }
    }
    state.pages.first().map(|page| page.target_id.clone())
}

async fn stream_page(
    session_id: String,
    cdp_url: String,
    page: PageRef,
    frames: broadcast::Sender<Arc<Frame>>,
    cache: Arc<Mutex<HashMap<String, Arc<Frame>>>>,
) -> Result<(), CdpError> {
    let mut viewer = Viewer::connect(&cdp_url, &page.target_id).await?;
    info!(
        session = %session_id,
        page = %page.target_id,
        url = %viewer.url(),
        "viewer attached"
    );

    let viewport = match viewer
        .command("Page.getLayoutMetrics", serde_json::json!({}))
        .await
    {
        Ok(value) => cdp::viewport_from_metrics(&value),
        Err(error) => {
            debug!(session = %session_id, error = %error, "layout metrics unavailable");
            None
        }
    };

    cdp::start_screencast(&viewer).await?;
    debug!(session = %session_id, page = %page.target_id, "screencast started");

    while let Some(event) = viewer.next_event().await {
        if event.method != "Page.screencastFrame" {
            continue;
        }
        debug!(session = %session_id, page = %page.target_id, "screencast frame received");
        let frame = match cdp::ScreencastFrame::parse(&event.params) {
            Ok(frame) => frame,
            Err(error) => {
                warn!(session = %session_id, error = %error, "dropping an undecodable frame");
                continue;
            }
        };

        // Ack before broadcasting: an unacked frame stalls every viewer.
        if let Err(error) = viewer
            .command(
                "Page.screencastFrameAck",
                serde_json::json!({"sessionId": frame.session_id}),
            )
            .await
        {
            debug!(session = %session_id, error = %error, "screencast ack failed");
        }

        let device_width = frame.metadata.device_width.max(1);
        let device_height = frame.metadata.device_height.max(1);
        let meta = FrameMeta {
            page_id: page.target_id.clone(),
            viewport_width: viewport.map_or(device_width, |viewport| viewport.width),
            viewport_height: viewport.map_or(device_height, |viewport| viewport.height),
            device_width,
            device_height,
            page_scale_factor: frame.metadata.page_scale_factor,
            scroll_offset_x: frame.metadata.scroll_offset_x,
            scroll_offset_y: frame.metadata.scroll_offset_y,
        };
        // Sending without receivers is not an error: the supervisor stops when
        // the last viewer leaves, and this frame may race that transition.
        let meta_page = meta.page_id.clone();
        let frame = Arc::new(Frame {
            jpeg: frame.jpeg,
            meta,
        });
        cache
            .lock()
            .expect("frame cache poisoned")
            .insert(meta_page.clone(), frame.clone());
        let receivers = frames.receiver_count();
        if frames.send(frame).is_err() {
            debug!(
                session = %session_id,
                page = %meta_page,
                receivers,
                "no viewer is listening for frames"
            );
        } else {
            debug!(
                session = %session_id,
                page = %meta_page,
                receivers,
                "frame broadcast"
            );
        }
    }

    Ok(())
}

/// MIME type for a `Page.captureScreenshot` payload.
///
/// `format: "jpeg"` is a request, not a guarantee: Kitesurf returns a small PNG
/// for an unpainted page such as `about:blank`. The data URL must follow the
/// magic bytes, or a strict client refuses to paint the tab thumbnail.
fn thumbnail_mime(data: &str) -> &'static str {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    // Six bytes are enough for both PNG (`89 50 4e 47 0d 0a`) and JPEG
    // (`ff d8 ff`).
    let prefix = &data[..data.len().min(8)];
    match STANDARD.decode(prefix) {
        Ok(bytes) if bytes.starts_with(&[0x89, b'P', b'N', b'G']) => "image/png",
        _ => "image/jpeg",
    }
}

async fn refresh_thumbs(
    cdp_url: String,
    pages: Vec<PageRef>,
    active: Option<String>,
    messages: broadcast::Sender<ServerMessage>,
) {
    for page in pages {
        if active.as_deref() == Some(page.target_id.as_str()) {
            continue;
        }
        let capture = tokio::time::timeout(
            THUMBNAIL_TIMEOUT,
            cdp::capture_thumbnail(&cdp_url, &page.target_id),
        )
        .await;
        match capture {
            Ok(Ok(data)) => {
                debug!(page = %page.target_id, bytes = data.len(), "thumbnail captured");
                let _ = messages.send(ServerMessage::Thumb {
                    page_id: page.target_id,
                    data: format!("data:{};base64,{data}", thumbnail_mime(&data)),
                });
            }
            Ok(Err(error)) => debug!(page = %page.target_id, error = %error, "thumbnail failed"),
            Err(_) => debug!(page = %page.target_id, "thumbnail timed out"),
        }
    }
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;
