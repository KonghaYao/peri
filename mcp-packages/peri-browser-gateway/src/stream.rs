//! 只读 CDP：共享当前活动页的一张最新帧；缩略图不占用 page writer。

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::{broadcast, watch};

use crate::cdp::{self, CdpError, Viewer};
use crate::core::BrowserCore;
use crate::page::PageHandle;
use crate::protocol::{ServerMessage, ViewStamp};
use crate::session::SettlementError;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const THUMBNAIL_TIMEOUT: Duration = Duration::from_millis(800);

pub(crate) struct Frame {
    pub jpeg: Bytes,
    pub meta: FrameMeta,
    pub stamp: ViewStamp,
}

impl Frame {
    pub fn meta_message(&self) -> ServerMessage<'_> {
        ServerMessage::Meta {
            stamp: &self.stamp,
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

#[derive(Clone, Debug)]
pub(crate) struct FrameMeta {
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub device_width: u32,
    pub device_height: u32,
    pub page_scale_factor: f64,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
}

pub(crate) struct Streams {
    pub frame: watch::Sender<Option<Arc<Frame>>>,
    pub thumb: broadcast::Sender<(String, String)>,
}

impl Streams {
    pub fn new() -> Self {
        Self {
            frame: watch::channel(None).0,
            thumb: broadcast::channel(16).0,
        }
    }
}

pub(crate) async fn run(core: Arc<BrowserCore>) -> Result<(), SettlementError> {
    let (screencast, thumbnails) = tokio::join!(screencasts(&core), thumbnails(&core));
    core.streams.frame.send_replace(None);
    screencast.and(thumbnails)
}

async fn screencasts(core: &BrowserCore) -> Result<(), SettlementError> {
    let mut changed = core.changed.subscribe();
    let mut failure = Ok(());
    while !core.stop.is_cancelled() {
        let page = core.view().page.filter(|_| core.viewer_counts().1 > 0);
        let Some(page) = page else {
            tokio::select! { _ = core.stop.cancelled() => break, _ = changed.changed() => {} }
            continue;
        };
        let obsolete = async {
            loop {
                if core.viewer_counts().1 == 0
                    || core.view().stamp.page_id.as_deref() != Some(&page.id)
                {
                    break;
                }
                let _ = changed.changed().await;
            }
        };
        let mut obsolete = std::pin::pin!(obsolete);
        let attached = tokio::select! {
            biased;
            _ = core.stop.cancelled() => break,
            _ = &mut obsolete => continue,
            result = Viewer::connect(&page.endpoint, &page.target_id) => result,
        };
        if let Ok(mut viewer) = attached {
            tokio::select! {
                biased;
                _ = core.stop.cancelled() => {},
                _ = &mut obsolete => {},
                _ = stream_page(core, &page, &mut viewer) => {},
            }
            if viewer.close().await.is_err() {
                failure = Err(SettlementError::Unconfirmed);
            }
            core.streams.frame.send_replace(None);
        }
        tokio::select! {
            biased;
            _ = core.stop.cancelled() => break,
            _ = async { if core.view().stamp.page_id.as_deref() == Some(&page.id) { tokio::time::sleep(RECONNECT_DELAY).await; } } => {},
        }
    }
    failure
}

async fn stream_page(
    core: &BrowserCore,
    page: &PageHandle,
    viewer: &mut Viewer,
) -> Result<(), CdpError> {
    let viewport = viewer
        .command("Page.getLayoutMetrics", json!({}))
        .await
        .ok()
        .and_then(|value| cdp::viewport_from_metrics(&value));
    cdp::start_screencast(viewer).await?;
    while let Some(event) = viewer.next_event().await {
        if event.method != "Page.screencastFrame" {
            continue;
        }
        let Ok(frame) = cdp::ScreencastFrame::parse(&event.params) else {
            continue;
        };
        let stamp = core.view().stamp;
        viewer
            .command(
                "Page.screencastFrameAck",
                json!({"sessionId": frame.session_id}),
            )
            .await?;
        if stamp.page_id.as_deref() != Some(&page.id) || core.view().stamp != stamp {
            continue;
        }
        let device_width = frame.metadata.device_width.max(1);
        let device_height = frame.metadata.device_height.max(1);
        let meta = FrameMeta {
            viewport_width: viewport.map_or(device_width, |viewport| viewport.width),
            viewport_height: viewport.map_or(device_height, |viewport| viewport.height),
            device_width,
            device_height,
            page_scale_factor: frame.metadata.page_scale_factor,
            scroll_offset_x: frame.metadata.scroll_offset_x,
            scroll_offset_y: frame.metadata.scroll_offset_y,
        };
        core.streams.frame.send_replace(Some(Arc::new(Frame {
            jpeg: frame.jpeg,
            meta,
            stamp,
        })));
    }
    Ok(())
}

async fn thumbnails(core: &BrowserCore) -> Result<(), SettlementError> {
    let mut changed = core.changed.subscribe();
    let mut previous = None;
    let mut failure = Ok(());
    while !core.stop.is_cancelled() {
        let (revision, active, pages) = core.stream_tabs();
        let visible = core.viewer_counts().1 > 0;
        let key = (revision, visible);
        if previous != Some(key) && visible {
            for page in pages {
                if core.stop.is_cancelled()
                    || core.stream_tabs().0 != revision
                    || core.viewer_counts().1 == 0
                {
                    break;
                }
                if active.as_deref() == Some(&page.id) || page.expired() {
                    continue;
                }
                let deadline = tokio::time::Instant::now() + THUMBNAIL_TIMEOUT;
                let result = tokio::select! {
                    biased;
                    _ = core.stop.cancelled() => break,
                    result = tokio::time::timeout_at(deadline, Viewer::connect(&page.endpoint, &page.target_id)) => result,
                };
                let Ok(Ok(mut viewer)) = result else { continue };
                let captured = tokio::select! {
                    biased;
                    _ = core.stop.cancelled() => None,
                    result = tokio::time::timeout_at(deadline, cdp::thumbnail(&viewer)) => result.ok().and_then(Result::ok),
                };
                if viewer.close().await.is_err() {
                    failure = Err(SettlementError::Unconfirmed);
                }
                if let Some(data) =
                    captured.filter(|_| !page.expired() && core.stream_tabs().0 == revision)
                {
                    let _ = core.streams.thumb.send((
                        page.id.clone(),
                        format!("data:{};base64,{data}", thumbnail_mime(&data)),
                    ));
                }
            }
        }
        previous = Some(key);
        tokio::select! { _ = core.stop.cancelled() => break, _ = changed.changed() => {} }
    }
    failure
}

fn thumbnail_mime(data: &str) -> &'static str {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    match data
        .as_bytes()
        .get(..8)
        .and_then(|prefix| STANDARD.decode(prefix).ok())
    {
        Some(bytes) if bytes.starts_with(&[0x89, b'P', b'N', b'G']) => "image/png",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;
