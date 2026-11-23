//! 监控连接的读侧独立观察 EOF；写侧串行发送状态与 Meta/JPEG 对。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::cdp::Viewport;
use crate::core::BrowserCore;
use crate::error::BrowserError;
use crate::protocol::{Command, ServerMessage, Signal};
use crate::stream::{Frame, FrameMeta};

const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_ENDED: u16 = 4001;
type Results = mpsc::Sender<(u64, Result<Value, BrowserError>)>;
type SentFrame = Mutex<Option<Arc<Frame>>>;

pub(crate) async fn handle(socket: WebSocket, core: Arc<BrowserCore>) {
    if core.add_viewer().is_err() {
        return;
    }
    let (sink, source) = socket.split();
    let client = core.stop.child_token();
    let (commands, pending) = mpsc::channel(1);
    let (results, replies) = mpsc::channel(16);
    let sent = SentFrame::default();
    let (visible, (), ()) = tokio::join!(
        receive(source, &core, &client, commands, results.clone()),
        execute(pending, &core, &client, results, &sent),
        publish(sink, &core, &client, replies, &sent),
    );
    core.remove_viewer(visible).await;
}

async fn receive(
    mut source: SplitStream<WebSocket>,
    core: &BrowserCore,
    client: &CancellationToken,
    tx: mpsc::Sender<Command>,
    replies: Results,
) -> bool {
    let _stop_on_leaving = client.clone().drop_guard();
    let mut visible = true;
    loop {
        let message = tokio::select! {
            biased;
            _ = client.cancelled() => break,
            message = source.next() => message,
        };
        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            _ => continue,
        };
        if let Ok(signal) = serde_json::from_str::<Signal>(text.as_str()) {
            if let Signal::Visible { visible: next } = signal {
                if visible != next {
                    core.viewer_visibility(next);
                    visible = next;
                }
            }
            continue;
        }
        let Ok(command) = serde_json::from_str::<Command>(text.as_str()) else {
            continue;
        };
        if let Err(error) = core.admit_monitor(&command, client) {
            if replies.try_send((command.id, Err(error))).is_err() {
                break;
            }
        } else if let Err(error) = tx.try_send(command) {
            if replies
                .try_send((error.into_inner().id, Err(BrowserError::Busy)))
                .is_err()
            {
                break;
            }
        }
    }
    visible
}

async fn execute(
    mut pending: mpsc::Receiver<Command>,
    core: &Arc<BrowserCore>,
    client: &CancellationToken,
    replies: Results,
    sent: &SentFrame,
) {
    let _stop_on_leaving = client.clone().drop_guard();
    loop {
        let command = tokio::select! {
            biased;
            _ = client.cancelled() => break,
            command = pending.recv() => match command { Some(command) => command, None => break },
        };
        let frame = sent
            .lock()
            .unwrap()
            .as_ref()
            .filter(|frame| frame.stamp == command.view)
            .cloned();
        let result = tokio::select! {
            biased;
            _ = client.cancelled() => break,
            result = tokio::time::timeout(core.config.structural_budget(), core.monitor_command(&command, client, frame.as_ref().map(|f| &f.meta))) =>
                result.unwrap_or_else(|_| Err(BrowserError::Timeout("monitor command".to_owned()))),
        };
        if replies.try_send((command.id, result)).is_err() {
            break;
        }
    }
}

async fn publish(
    mut sink: SplitSink<WebSocket, Message>,
    core: &BrowserCore,
    client: &CancellationToken,
    mut replies: mpsc::Receiver<(u64, Result<Value, BrowserError>)>,
    sent: &SentFrame,
) {
    let _stop_on_leaving = client.clone().drop_guard();
    let send = async {
        let mut changed = core.changed.subscribe();
        let mut frames = core.streams.frame.subscribe();
        let mut thumbs = core.streams.thumb.subscribe();
        let mut sent_state: Option<Value> = None;
        send_json(
            &mut sink,
            &json!({"t": "hello", "session": core.session_id(), "name": core.name()}),
        )
        .await?;
        loop {
            let (stamp, model) = core.monitor_state().await;
            if sent_state.as_ref() != Some(&model) && core.view().stamp == stamp {
                send_json(&mut sink, &model).await?;
                sent_state = Some(model);
            }
            let frame = frames
                .borrow_and_update()
                .as_ref()
                .filter(|f| f.stamp == stamp)
                .cloned();
            if let Some(frame) = frame.filter(|frame| {
                core.view().stamp == frame.stamp
                    && !sent
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|last| Arc::ptr_eq(last, frame))
            }) {
                send_json(&mut sink, &frame.meta_message()).await?;
                send_message(&mut sink, Message::Binary(frame.jpeg.clone())).await?;
                *sent.lock().unwrap() = Some(frame);
            }
            tokio::select! {
                biased;
                reply = replies.recv() => {
                    let Some((id, reply)) = reply else { return Ok::<_, ()>(()); };
                    let message = match reply {
                        Ok(value) => ServerMessage::Result { id, ok: true, value, code: None },
                        Err(error) => ServerMessage::Result { id, ok: false, value: Value::Null, code: Some(error.code()) },
                    };
                    send_json(&mut sink, &message).await?;
                }
                _ = changed.changed() => {},
                _ = frames.changed() => {},
                thumb = thumbs.recv() => {
                    if let Ok((id, data)) = thumb {
                        if core.stream_tabs().2.iter().any(|page| page.id == id) {
                            send_json(&mut sink, &ServerMessage::Thumb { page_id: &id, data: &data }).await?;
                        }
                    }
                }
            }
        }
    };
    tokio::select! { biased; _ = client.cancelled() => {}, _ = send => {} }
    if core.stop.is_cancelled() {
        let _ = send_message(
            &mut sink,
            Message::Close(Some(CloseFrame {
                code: SESSION_ENDED,
                reason: "session ended".into(),
            })),
        )
        .await;
    }
}

async fn send_json(
    sink: &mut SplitSink<WebSocket, Message>,
    message: &impl Serialize,
) -> Result<(), ()> {
    let data = serde_json::to_string(message).map_err(|_| ())?;
    send_message(sink, Message::Text(data.into())).await
}

async fn send_message(
    sink: &mut SplitSink<WebSocket, Message>,
    message: Message,
) -> Result<(), ()> {
    tokio::time::timeout(SEND_TIMEOUT, sink.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

/// 用帧的设备像素与当前 viewport 将坐标转换、限制在页面内。
pub(crate) fn canvas_to_css(
    x: f64,
    y: f64,
    meta: Option<&FrameMeta>,
    layout: Option<Viewport>,
) -> (f64, f64) {
    let Some(meta) = meta.filter(|m| m.device_width > 0 && m.device_height > 0) else {
        return (x, y);
    };
    let layout = layout.filter(|viewport| viewport.width > 0 && viewport.height > 0);
    let width = f64::from(layout.map_or(meta.viewport_width, |viewport| viewport.width));
    let height = f64::from(layout.map_or(meta.viewport_height, |viewport| viewport.height));
    (
        (x * width / f64::from(meta.device_width)).clamp(0.0, (width - 1.0).max(0.0)),
        (y * height / f64::from(meta.device_height)).clamp(0.0, (height - 1.0).max(0.0)),
    )
}

#[cfg(test)]
#[path = "ws_test.rs"]
mod tests;
