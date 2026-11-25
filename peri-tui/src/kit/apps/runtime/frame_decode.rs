//! 帧解码：单槽最新帧 + 独立解码任务。
//!
//! 帧只用于显示。SSE 事件循环（`view_task::stream_view`）把帧放进只有一格的
//! 槽位后立即继续读取事件；base64/JPEG 解码在独立任务里进行。解码期间到达的
//! 新帧覆盖槽里的旧帧（允许合并/丢弃过期帧），因此 `app-request`、`closed`
//! 与错误事件不会排在帧解码之后。
//!
//! 内存有界：槽位只保留一帧，视图状态只保留解码后的最新一帧（`apply_frame`
//! 按 `seq` 去重）。视图隔离：每个 view 各自一个槽位与解码任务。
//!
//! 生命周期：[`FrameDecoder::finish`] 先解完在途帧再退出；超过预算则 abort，
//! 不泄漏任务（`spawn_blocking` 的解码线程随后自行结束）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::super::state::{self, FrameImage};
use super::super::view_client::{FrameEvent, decode_frame_bytes};

/// 帧 JPEG 的输入上限与解压后尺寸上限（防解码炸弹）。
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAME_DIMENSION: u32 = 8192;

/// 解码实现：生产是 [`jpeg_decoder`]；测试注入可控时序。
pub(super) type FrameDecodeFn = Arc<dyn Fn(&FrameEvent) -> Option<FrameImage> + Send + Sync>;

/// 生产用解码器（JPEG）。
pub(super) fn jpeg_decoder() -> FrameDecodeFn {
    Arc::new(decode_frame)
}

/// 单槽最新帧 + 后台解码任务。
pub(super) struct FrameDecoder {
    slot: Arc<Mutex<Option<Arc<FrameEvent>>>>,
    wake: Arc<Notify>,
    /// 视图收尾：不再接收新帧，解完在途帧后退出。
    closed: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl FrameDecoder {
    pub(super) fn start(
        view_id: impl Into<String>,
        cancel: CancellationToken,
        decode: FrameDecodeFn,
    ) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let wake = Arc::new(Notify::new());
        let closed = Arc::new(AtomicBool::new(false));
        let join = tokio::spawn(decode_frames(
            view_id.into(),
            Arc::clone(&slot),
            Arc::clone(&wake),
            Arc::clone(&closed),
            cancel,
            decode,
        ));
        Self {
            slot,
            wake,
            closed,
            join: Some(join),
        }
    }

    /// 非阻塞投放：槽里只保留最新帧；未被取走的旧帧被覆盖。
    pub(super) fn offer(&self, frame: Arc<FrameEvent>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        *self.slot.lock() = Some(frame);
        self.wake.notify_one();
    }

    /// 结束解码任务：解完（或丢弃）槽内最后一帧后退出。
    ///
    /// 超过 `timeout` 说明卡在解码线程上，直接 abort——视图收尾不被帧解码拖住。
    pub(super) async fn finish(&mut self, timeout: Duration) {
        self.closed.store(true, Ordering::Release);
        let Some(mut join) = self.join.take() else {
            return;
        };
        self.wake.notify_one();
        if tokio::time::timeout(timeout, &mut join).await.is_err() {
            join.abort();
        }
    }
}

async fn decode_frames(
    view_id: String,
    slot: Arc<Mutex<Option<Arc<FrameEvent>>>>,
    wake: Arc<Notify>,
    closed: Arc<AtomicBool>,
    cancel: CancellationToken,
    decode: FrameDecodeFn,
) {
    loop {
        // 先取走槽里最新的一帧：解码期间被覆盖的中间帧不会进入队列。
        let frame = slot.lock().take();
        if let Some(frame) = frame {
            let decode = Arc::clone(&decode);
            let decoded = tokio::task::spawn_blocking(move || decode(&frame))
                .await
                .ok()
                .flatten();
            if cancel.is_cancelled() {
                return;
            }
            if let Some(image) = decoded {
                state::update_view(&view_id, |view| view.apply_frame(image));
            }
            continue;
        }
        if closed.load(Ordering::Acquire) {
            return;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = wake.notified() => {}
        }
    }
}

/// base64 + JPEG 解码；非法或超限的帧返回 `None`（丢弃，不影响事件流）。
fn decode_frame(frame: &FrameEvent) -> Option<FrameImage> {
    let bytes = decode_frame_bytes(frame).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return None;
    }
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    if decoded.width() > MAX_FRAME_DIMENSION || decoded.height() > MAX_FRAME_DIMENSION {
        return None;
    }
    Some(FrameImage {
        seq: frame.seq,
        width: decoded.width(),
        height: decoded.height(),
        viewport_width: frame.viewport_width,
        viewport_height: frame.viewport_height,
        image: Arc::new(decoded),
    })
}

#[cfg(test)]
#[path = "frame_decode_test.rs"]
mod tests;
