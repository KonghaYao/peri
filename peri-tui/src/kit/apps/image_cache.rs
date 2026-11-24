//! 面板帧编码缓存：把最新 JPEG 帧在后台线程编码成终端图片协议。
//!
//! 与 composer 附件预览同一机制（`input_area::attachment_preview`）：
//! render body 只查表 + 写预留几何，base64 解码/尺寸校验/协议编码全部在
//! effect 启动的后台线程完成；完成时 bump `RENDER_HEARTBEAT` 唤醒重渲染。
//!
//! 单条在途编码：帧流比编码快时丢弃中间帧，只追最新帧（实时视图语义）。
//! 缓存键含 view_id + seq + 目标单元格，切换视图/尺寸后旧结果不会画到新帧上。

use std::sync::{Arc, OnceLock};

use crate::kit::atoms::RENDER_HEARTBEAT;
use crate::kit::image_preview::{InlineImage, InlineImageEncoder, protocol_available};
use crate::kit::terminal_caps::TerminalCaps;
use image::DynamicImage;
use parking_lot::Mutex;

/// 帧编码键：同一视图、同一帧、同一目标面积才可复用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameKey {
    pub view_id: String,
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Default)]
struct Cache {
    ready: Option<(FrameKey, Arc<InlineImage>)>,
    inflight: Option<FrameKey>,
    failed: Option<FrameKey>,
    revision: u64,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// 纯查表：当前键已编码完成时返回协议句柄。
#[must_use]
pub fn lookup_ready(key: &FrameKey) -> Option<Arc<InlineImage>> {
    let cache = cache().lock();
    cache
        .ready
        .as_ref()
        .filter(|(ready_key, _)| ready_key == key)
        .map(|(_, image)| Arc::clone(image))
}

/// 当前键编码失败（面板据此显示降级文案，不反复重试）。
#[must_use]
pub fn is_failed(key: &FrameKey) -> bool {
    cache()
        .lock()
        .failed
        .as_ref()
        .is_some_and(|failed| failed == key)
}

/// effect 边界：请求编码当前帧。
///
/// 终端无图片协议（None / ITerm2 / 缺单元格尺寸的 Sixel）时直接返回，
/// 不构造 `Picker`（tmux 下 `Picker::halfblocks` 有 spawn 副作用）。
pub fn request(
    key: FrameKey,
    image: Arc<DynamicImage>,
    caps: &TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    if !protocol_available(caps, cell_px) {
        return;
    }
    let Some(encoder) = InlineImageEncoder::for_caps(caps, cell_px) else {
        return;
    };

    let cols = key.cols;
    let rows = key.rows;
    {
        let mut cache = cache().lock();
        // 单条在途编码：帧流更快时丢弃中间帧；完成时 revision +1 让 effect
        // 以最新帧重新请求（否则 deps 不变，stale 帧会永久停留）。
        if cache.inflight.is_some() {
            return;
        }
        if cache.ready.as_ref().is_some_and(|(ready, _)| ready == &key) {
            return;
        }
        // 同一视图已经有更新的帧就绪：旧帧请求直接丢弃（乱序完成防御）。
        if let Some((ready, _)) = cache.ready.as_ref()
            && ready.view_id == key.view_id
            && ready.seq > key.seq
        {
            return;
        }
        if cache.failed.as_ref() == Some(&key) {
            return;
        }
        cache.inflight = Some(key.clone());
    }

    let thread_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("peri-app-frame".to_string())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encoder.encode((*image).clone(), cols, rows)
            }))
            .unwrap_or(None);
            complete(thread_key, result.map(Arc::new));
        });
    if spawned.is_err() {
        let mut cache = cache().lock();
        cache.inflight = None;
        cache.failed = Some(key);
    }
}

fn complete(key: FrameKey, image: Option<Arc<InlineImage>>) {
    let updated = {
        let mut cache = cache().lock();
        if cache.inflight.as_ref() == Some(&key) {
            cache.inflight = None;
        }
        match image {
            Some(image) => {
                // 较旧帧不得覆盖较新帧（同一视图按 seq 单调）。
                let stale = cache
                    .ready
                    .as_ref()
                    .is_some_and(|(ready, _)| ready.view_id == key.view_id && ready.seq > key.seq);
                if stale {
                    false
                } else {
                    cache.ready = Some((key, image));
                    cache.revision = cache.revision.wrapping_add(1);
                    true
                }
            }
            None => {
                cache.failed = Some(key);
                cache.revision = cache.revision.wrapping_add(1);
                true
            }
        }
    };
    if updated {
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
}

/// 编码版本号：成功/失败完成时 +1。
///
/// 面板把它放进 `use_effect` 的 deps：在途期间被丢弃的最新帧，会在本次编码
/// 完成后触发一次新的 effect，避免停在中间帧。
#[must_use]
pub fn revision() -> u64 {
    cache().lock().revision
}

#[cfg(test)]
#[path = "image_cache_test.rs"]
mod tests;
