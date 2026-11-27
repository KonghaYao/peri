//! 统一图片缓存：编码结果、上传仓库、后台编码与重绘版本。
//!
//! 三处 surface（转录区/用户气泡/工具卡/composer 预览）共用同一缓存：
//!
//! - 渲染 body 只调用 [`lookup`]（纯查表、无 IO）；
//! - 请求由 effect 边界调用 [`request`]：解码/编码在后台线程，完成后 bump 全局
//!   [`revision`] 并唤醒 `RENDER_HEARTBEAT`；
//! - 上传图片的 base64 只活在进程内 [`retain_upload`] 仓库（LRU，[`MAX_CACHE_BYTES`]），
//!   视图模型只携带 [`UploadId`]；
//! - 缓存键 = 来源版本 + 预算 + 铺满策略 + **单元格像素**：字体/缩放变化后同一
//!   `(cols, rows)` 必须重编码，否则像素密度与宽高比停留在旧值。
//!
//! 失败与淘汰一律回落到调用方的文本/meta 降级，不向用户暴露原因。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use image::DynamicImage;
use image::GenericImageView as _;
use parking_lot::Mutex;

use crate::kit::atoms::RENDER_HEARTBEAT;
use crate::kit::image_preview::{InlineImage, InlineImageEncoder};
use crate::kit::image_safety::{
    ImageSafetyError, MAX_CACHE_BYTES, MAX_IMAGE_BYTES, MAX_IMAGE_SIDE, MAX_TOTAL_PIXELS,
    read_validated_image,
};
use crate::kit::terminal_caps::TerminalCaps;

use super::sizing::{self, Budget, Fill};
use super::source::{
    ImageSource, SourceRef, UploadId, UploadMedia, file_version, resolve_local_path,
};

/// 同时解码的后台线程上限；超出的请求留待下一次 effect 重试。
const MAX_CONCURRENT: usize = 4;

/// 缓存条目上限（零字节失败与同一文件的多个版本也要有界）。
const MAX_CACHE_ENTRIES: usize = 512;

/// 别名上限（不同写法指向同一文件版本）。
const MAX_SOURCE_ALIASES: usize = 1024;

/// 瞬态失败（线程 spawn 失败 / 解码 panic）的自动重试次数上限。
///
/// 这类失败不改变来源内容，下一次请求有机会成功；但无限重试会在每次 revision
/// 唤醒时反复起线程，所以到上限后固定降级为 `Failed`（文本/meta 兜底）。
const MAX_TRANSIENT_RETRIES: u8 = 1;

/// 失败原因是否可能由重试恢复（spawn 资源不足、后台线程 panic）。
fn is_transient_failure(reason: &'static str) -> bool {
    matches!(reason, "spawn" | "panic")
}

/// 协议缓存与上传仓库**各自**的字节预算（两套独立账本）。
///
/// 两者都取 [`MAX_CACHE_BYTES`]（64 MiB），但互不抵扣：协议缓存驻留编码结果，
/// 上传仓库驻留 base64 载荷。同时打满时进程峰值为 128 MiB，不是 64 MiB。
const BYTE_BUDGET: usize = MAX_CACHE_BYTES as usize;

/// 渲染期可用的图片：协议句柄 + 真实占用单元格 + 水平居中偏移。
#[derive(Debug, Clone)]
pub struct Placement {
    pub cols: u16,
    pub rows: u16,
    /// 水平居中偏移（相对预算区域左边）。
    pub x_offset: u16,
    pub image: Arc<InlineImage>,
}

/// 三态缓存值：渲染 body 只克隆 [`Arc`]，不阻塞。
#[derive(Clone)]
enum ImageState {
    Pending,
    Ready(Arc<InlineImage>),
    /// 失败原因只用于诊断与单测；渲染统一回落降级，不向用户展示。
    #[allow(dead_code)]
    Failed(&'static str),
}

impl std::fmt::Debug for ImageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Ready(image) => f.debug_tuple("Ready").field(image).finish(),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

/// 版本键：来源版本 + 预算 + 单元格像素。
///
/// 本缓存统一按 [`Fill::Upscale2x`] 铺满（转录区/气泡/工具卡/composer 预览同一
/// 策略）；需要「不放大」的 surface（@image 交互浮层）走 `AsyncImage`，不进本缓存。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    source: SourceId,
    cols: u16,
    max_rows: u16,
    cell_px: (u16, u16),
}

/// 来源版本：同路径 mtime/len 变化、上传摘要碰撞都视为不同图片。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SourceId {
    File {
        path: PathBuf,
        mtime: u128,
        len: u64,
    },
    Uploaded {
        digest: u64,
        /// 媒体类型是上传身份的一部分：同一摘要按不同 mime 解码是不同编码。
        media: UploadMedia,
        /// 会话内摘要碰撞（不同载荷、同一 FNV 摘要）时递增，让旧编码结果不可达。
        ///
        /// 它**不保证不串图**：视图模型只携带摘要 + mime，碰撞后旧气泡与新气泡
        /// 指向同一仓库槽位，旧气泡可能显示新载荷；epoch 只保证不会继续画出
        /// 旧载荷编码出来的尺寸/像素。
        epoch: u64,
    },
}

struct Alias {
    key: CacheKey,
    last_used: u64,
}

struct Entry {
    state: ImageState,
    last_used: u64,
    bytes: usize,
    /// 已消耗的瞬态失败重试次数（成功或确定性失败后不再使用）。
    transient_retries: u8,
}

impl Entry {
    fn pending(last_used: u64) -> Self {
        Self {
            state: ImageState::Pending,
            last_used,
            bytes: 0,
            transient_retries: 0,
        }
    }
}

struct Upload {
    payload: Arc<str>,
    bytes: usize,
    last_used: u64,
}

/// [`Cache::begin`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginOutcome {
    /// 调用方应启动后台线程。
    Spawn(u64),
    /// 同版本同尺寸已有条目（含 Pending/Failed），只更新别名与 LRU。
    Existing,
    /// 并发已满；本次不登记，等下一次 effect 重试。
    Deferred,
}

/// [`Cache::complete`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompleteOutcome {
    Updated,
    /// 瞬态失败：槽位已释放，条目留在 Pending 等待下一次 effect 重试。
    Retrying,
    /// 槽位已释放，但结果被丢弃（来源已指向新版本）。
    Released,
    /// 非当前请求（已被同键新请求取代），无槽位变化。
    Ignored,
}

impl CompleteOutcome {
    /// 是否需要唤醒渲染 effect。释放槽位也必须唤醒：否则并发暂缓的图片
    /// 要等到滚动/内容变化才会重试，期间一直停留在文本降级。
    fn wakes_render(self) -> bool {
        !matches!(self, Self::Ignored)
    }
}

/// 单进程缓存本体。独立成结构体便于单测注入键与预算。
#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, Entry>,
    aliases: HashMap<SourceRef, Alias>,
    inflight: HashMap<CacheKey, u64>,
    next_request: u64,
    clock: u64,
    bytes: usize,
    revision: u64,
    uploads: HashMap<u64, Upload>,
    upload_bytes: usize,
    upload_epoch: u64,
    /// effect 边界最近一次观测到的单元格像素尺寸；render 查表用它构造来源键，
    /// 不在每帧做 ioctl（TUI-RENDER-001）。`None` 表示尚未观测，一律未命中。
    observed_cell_px: Option<(u16, u16)>,
    /// 单测钩子：让接下来 N 次 `begin` 返回 `Deferred`（模拟并发槽位已满）。
    /// 不占用真实 inflight，避免污染其它并发图片测试。
    #[cfg(test)]
    defer_requests: usize,
    /// 单测钩子：暂缓发生后立即 bump revision（模拟槽位在同一帧内释放），
    /// 让持久组件树的下一帧能观察到 deps 变化并重试。
    #[cfg(test)]
    signal_release_after_defer: bool,
}

impl Cache {
    fn lookup(&mut self, source: SourceRef) -> Option<ImageState> {
        let alias = self.aliases.get_mut(&source)?;
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        alias.last_used = now;
        let key = alias.key.clone();
        let Some(entry) = self.entries.get_mut(&key) else {
            self.aliases.remove(&source);
            return None;
        };
        entry.last_used = now;
        Some(entry.state.clone())
    }

    /// 登记一次请求；`Spawn` 表示调用方应 spawn 后台线程。
    fn begin(&mut self, source: SourceRef, key: CacheKey) -> BeginOutcome {
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        #[cfg(test)]
        if self.defer_requests > 0 {
            self.defer_requests -= 1;
            self.aliases.remove(&source);
            if self.signal_release_after_defer {
                self.signal_release_after_defer = false;
                REVISION.fetch_add(1, Ordering::SeqCst);
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
            }
            return BeginOutcome::Deferred;
        }
        // 完成态条目（Ready/Failed）只更新别名与 LRU；Pending 条目可能来自瞬态
        // 失败（无在途请求），需要允许重新派发，否则一次 spawn 失败会永久降级。
        let finished = self
            .entries
            .get(&key)
            .is_some_and(|entry| !matches!(entry.state, ImageState::Pending));
        let in_flight = self.inflight.contains_key(&key);
        if !finished && !in_flight && self.inflight.len() >= MAX_CONCURRENT {
            // A stale version must stop drawing even when its replacement cannot start yet.
            self.aliases.remove(&source);
            return BeginOutcome::Deferred;
        }
        self.aliases.insert(
            source,
            Alias {
                key: key.clone(),
                last_used: now,
            },
        );
        if self.aliases.len() > MAX_SOURCE_ALIASES {
            let oldest = self
                .aliases
                .iter()
                .min_by_key(|(_, alias)| alias.last_used)
                .map(|(source, _)| source.clone());
            if let Some(oldest) = oldest {
                self.aliases.remove(&oldest);
            }
        }
        if finished || in_flight {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.last_used = now;
            }
            return BeginOutcome::Existing;
        }
        match self.entries.get_mut(&key) {
            Some(entry) => entry.last_used = now,
            None => {
                self.entries.insert(key.clone(), Entry::pending(now));
            }
        }
        self.next_request = self.next_request.wrapping_add(1);
        let id = self.next_request;
        self.inflight.insert(key, id);
        self.evict_to_budget(BYTE_BUDGET);
        BeginOutcome::Spawn(id)
    }

    /// 写回后台结果；请求代数不匹配（被更新请求取代）或别名已指向别的版本
    /// 时丢弃。
    fn complete(
        &mut self,
        key: &CacheKey,
        id: u64,
        result: Result<Arc<InlineImage>, &'static str>,
    ) -> CompleteOutcome {
        if self.inflight.get(key).copied() != Some(id) {
            return CompleteOutcome::Ignored;
        }
        self.inflight.remove(key);
        if !self.aliases.values().any(|alias| alias.key == *key) {
            // 该版本已被新请求取代：清掉遗留的 Pending 条目，否则同一文件版本
            // 再次出现时 `begin` 会因条目存在而永不重启 worker（永久文本降级）。
            self.entries.remove(key);
            return CompleteOutcome::Released;
        }
        let Some(entry) = self.entries.get_mut(key) else {
            return CompleteOutcome::Released;
        };
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        // 瞬态失败（spawn 失败 / 解码 panic）先回 Pending，让 effect 在下一次
        // revision 唤醒时重试；到重试上限后才固定为 Failed，避免无限起线程。
        let retrying = matches!(&result, Err(reason) if is_transient_failure(reason))
            && entry.transient_retries < MAX_TRANSIENT_RETRIES;
        let (state, bytes) = match result {
            Ok(image) => {
                entry.transient_retries = 0;
                let bytes = image.retained_bytes();
                (ImageState::Ready(image), bytes)
            }
            Err(_) if retrying => {
                entry.transient_retries = entry.transient_retries.saturating_add(1);
                (ImageState::Pending, 0)
            }
            Err(reason) => (ImageState::Failed(reason), 0),
        };
        entry.state = state;
        entry.bytes = bytes;
        self.clock = self.clock.wrapping_add(1);
        entry.last_used = self.clock;
        self.bytes = self.bytes.saturating_add(bytes);
        self.revision = self.revision.wrapping_add(1);
        self.evict_to_budget(BYTE_BUDGET);
        if retrying {
            CompleteOutcome::Retrying
        } else {
            CompleteOutcome::Updated
        }
    }

    fn evict_to_budget(&mut self, budget: usize) {
        while self.bytes > budget || self.entries.len() > MAX_CACHE_ENTRIES {
            let victim = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !matches!(entry.state, ImageState::Pending)
                        && (self.bytes <= budget || entry.bytes > 0)
                })
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            let Some(victim) = victim else {
                break;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
            self.aliases.retain(|_, alias| alias.key != victim);
        }
    }

    fn upload(&mut self, digest: u64) -> Option<Arc<str>> {
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        let upload = self.uploads.get_mut(&digest)?;
        upload.last_used = now;
        Some(Arc::clone(&upload.payload))
    }

    /// 登记上传载荷。同摘要不同载荷视为碰撞：丢弃旧值并递增 epoch，让旧编码
    /// 结果不可达（`SourceRef::Uploaded` 含 epoch）。
    ///
    /// 注意 epoch 的边界：它只让**旧编码**失效，不保证不会串图——视图模型只带
    /// 摘要 + mime，碰撞后仍指向同一仓库槽位，旧气泡可能改为显示新载荷；这是
    /// 64 位 FNV 进程内身份的成本，不是可依赖的正确性边界。
    fn retain_upload(&mut self, id: UploadId, payload: Arc<str>) {
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        if let Some(existing) = self.uploads.get_mut(&id.digest) {
            if existing.payload.as_ref() == payload.as_ref() {
                existing.last_used = now;
                return;
            }
            if let Some(removed) = self.uploads.remove(&id.digest) {
                self.upload_bytes = self.upload_bytes.saturating_sub(removed.bytes);
            }
            self.upload_epoch = self.upload_epoch.wrapping_add(1);
        }
        let bytes = payload.len();
        self.uploads.insert(
            id.digest,
            Upload {
                payload,
                bytes,
                last_used: now,
            },
        );
        self.upload_bytes = self.upload_bytes.saturating_add(bytes);
        self.evict_uploads();
    }

    fn evict_uploads(&mut self) {
        while self.upload_bytes > BYTE_BUDGET && self.uploads.len() > 1 {
            let victim = self
                .uploads
                .iter()
                .min_by_key(|(_, upload)| upload.last_used)
                .map(|(digest, _)| *digest);
            let Some(victim) = victim else {
                break;
            };
            if let Some(removed) = self.uploads.remove(&victim) {
                self.upload_bytes = self.upload_bytes.saturating_sub(removed.bytes);
            }
            self.aliases.retain(|source, _| {
                !matches!(source, SourceRef::Uploaded { digest, .. } if *digest == victim)
            });
        }
    }

    /// 会话边界：清空上传仓库与它的编码结果（上传图片不跨会话泄漏）。
    ///
    /// 本地文件图片按内容寻址、与会话无关，保留缓存避免重开会话时重复解码。
    fn release_all_uploads(&mut self) -> bool {
        let had = !self.uploads.is_empty()
            || self
                .entries
                .keys()
                .any(|key| matches!(key.source, SourceId::Uploaded { .. }));
        self.uploads.clear();
        self.upload_bytes = 0;
        self.entries
            .retain(|key, _| !matches!(key.source, SourceId::Uploaded { .. }));
        self.aliases
            .retain(|source, _| !matches!(source, SourceRef::Uploaded { .. }));
        self.bytes = self.entries.values().map(|entry| entry.bytes).sum();
        if had {
            self.revision = self.revision.wrapping_add(1);
        }
        had
    }
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static REVISION: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// 缓存版本；`EntryRenderCache` 据此在图片完成后重建对应消息。
#[must_use]
pub(crate) fn revision() -> u64 {
    REVISION.load(Ordering::SeqCst)
}

fn source_ref(
    source: &ImageSource<'_>,
    epoch: u64,
    budget: Budget,
    cell_px: (u16, u16),
) -> SourceRef {
    match source {
        ImageSource::File(raw) => SourceRef::File {
            source: raw.trim().to_string(),
            cols: budget.cols,
            max_rows: budget.max_rows,
            cell_px,
        },
        ImageSource::Uploaded(id) => SourceRef::Uploaded {
            digest: id.digest,
            media: id.media,
            epoch,
            cols: budget.cols,
            max_rows: budget.max_rows,
            cell_px,
        },
    }
}

/// 纯查表：命中 Ready 返回协议句柄与几何，Pending/Failed/未知一律 `None`。
/// 调用方（render body）不得在此函数外做任何 IO。
#[must_use]
pub(crate) fn lookup(source: &ImageSource<'_>, budget: Budget) -> Option<Placement> {
    if budget.is_empty() {
        return None;
    }
    let mut guard = cache().lock();
    let Some(cell_px) = guard.observed_cell_px else {
        return None;
    };
    let epoch = guard.upload_epoch;
    let source = source_ref(source, epoch, budget, cell_px);
    let state = guard.lookup(source)?;
    match state {
        ImageState::Ready(image) => Some(Placement {
            cols: image.cols(),
            rows: image.rows(),
            x_offset: budget.cols.saturating_sub(image.cols()) / 2,
            image,
        }),
        ImageState::Pending | ImageState::Failed(_) => None,
    }
}

/// 上传载荷是否仍在仓库里（render 只读内存状态，无 IO）。
///
/// 载荷缺失（会话边界清理 / LRU 淘汰 / 摘要未登记）时图片永远不会就绪，
/// 调用方应显示降级 meta 行而不是把它当作「编码中」无限等待。
#[must_use]
pub(crate) fn upload_available(id: UploadId) -> bool {
    cache().lock().uploads.contains_key(&id.digest)
}

/// 编码是否已失败：失败不预留高度，避免永久空白条。
#[must_use]
pub(crate) fn failed(source: &ImageSource<'_>, budget: Budget) -> bool {
    if budget.is_empty() {
        return false;
    }
    let mut guard = cache().lock();
    let Some(cell_px) = guard.observed_cell_px else {
        return false;
    };
    let epoch = guard.upload_epoch;
    let source = source_ref(source, epoch, budget, cell_px);
    matches!(guard.lookup(source), Some(ImageState::Failed(_)))
}

/// 非阻塞请求：文件经校验读取、上传经仓库取载荷，解码/编码在后台线程完成。
/// 重复请求由缓存去重；协议不可用/预算为空/来源无效时不做任何事。
pub(crate) fn request(
    source: ImageSource<'_>,
    budget: Budget,
    caps: &TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    if budget.is_empty() {
        return;
    }
    // 协议不可用时在构造 Picker 之前返回（`Picker::halfblocks` 有 tmux 副作用）。
    let Some(encoder) = InlineImageEncoder::for_caps(caps, cell_px) else {
        return;
    };
    let cell_px = cell_px
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or_else(|| encoder.font_size());
    let (source_ref, source_id, upload, path) = {
        let mut guard = cache().lock();
        // 这里是 effect 边界：记下当帧单元格尺寸，render 查表据此构造键，
        // 字体/缩放变化后不会再命中旧 cell_px 的编码结果。
        guard.observed_cell_px = Some(cell_px);
        match source {
            ImageSource::File(raw) => {
                let Some(path) = resolve_local_path(raw) else {
                    return;
                };
                let (path, mtime, len) = file_version(path);
                let source_ref = SourceRef::File {
                    source: raw.trim().to_string(),
                    cols: budget.cols,
                    max_rows: budget.max_rows,
                    cell_px,
                };
                let id = SourceId::File {
                    path: path.clone(),
                    mtime,
                    len,
                };
                (source_ref, id, None, Some(path))
            }
            ImageSource::Uploaded(id) => {
                let epoch = guard.upload_epoch;
                let Some(payload) = guard.upload(id.digest) else {
                    return;
                };
                let source_ref = SourceRef::Uploaded {
                    digest: id.digest,
                    media: id.media,
                    epoch,
                    cols: budget.cols,
                    max_rows: budget.max_rows,
                    cell_px,
                };
                (
                    source_ref,
                    SourceId::Uploaded {
                        digest: id.digest,
                        media: id.media,
                        epoch,
                    },
                    Some(payload),
                    None,
                )
            }
        }
    };
    let key = CacheKey {
        source: source_id,
        cols: budget.cols,
        max_rows: budget.max_rows,
        cell_px,
    };
    let id = match cache().lock().begin(source_ref, key.clone()) {
        BeginOutcome::Spawn(id) => id,
        BeginOutcome::Existing | BeginOutcome::Deferred => return,
    };
    let thread_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("peri-image-encode".to_string())
        .spawn(move || {
            // 解码/编码 panic 也要回收 inflight 槽位，否则并发上限会被永久占满。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encode(
                    &encoder,
                    upload.as_deref(),
                    path.as_deref(),
                    cell_px,
                    budget,
                )
            }))
            .unwrap_or(Err("panic"));
            finish(thread_key, id, result);
        });
    if spawned.is_err() {
        finish(key, id, Err("spawn"));
    }
}

/// 登记一次上传载荷（提交/工具事件边界调用）。
pub(crate) fn retain_upload(id: UploadId, payload: Arc<str>) {
    if payload.is_empty() {
        return;
    }
    cache().lock().retain_upload(id, payload);
}

/// 会话边界清空上传仓库与它的编码结果（本地文件缓存保留）。
pub(crate) fn release_all_uploads() {
    if cache().lock().release_all_uploads() {
        REVISION.fetch_add(1, Ordering::SeqCst);
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
}

fn finish(key: CacheKey, id: u64, result: Result<Arc<InlineImage>, &'static str>) {
    // 版本号与 Ready 状态必须在同一把锁内发布：否则渲染缓存可能先看到完成态，
    // 再读到旧的全局版本，判为未变化。丢弃结果也释放了槽位，同样要唤醒 effect。
    let mut guard = cache().lock();
    if guard.complete(&key, id, result).wakes_render() {
        REVISION.fetch_add(1, Ordering::SeqCst);
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
}

fn encode(
    encoder: &InlineImageEncoder,
    upload: Option<&str>,
    path: Option<&std::path::Path>,
    cell_px: (u16, u16),
    budget: Budget,
) -> Result<Arc<InlineImage>, &'static str> {
    let image = match (upload, path) {
        (Some(payload), _) => decode_upload(payload)?,
        (None, Some(path)) => {
            let (_meta, buffer) = read_validated_image(path).map_err(safety_reason)?;
            decode_image(buffer)?
        }
        (None, None) => return Err("source"),
    };
    let cells = sizing::fit(
        image.width(),
        image.height(),
        cell_px,
        budget,
        Fill::Upscale2x,
    )
    .ok_or("geometry")?;
    encoder
        .encode_scaled(image, cells.cols, cells.rows)
        .map(Arc::new)
        .ok_or("encode")
}

fn decode_upload(payload: &str) -> Result<DynamicImage, &'static str> {
    use base64::Engine as _;
    // 先从 base64 字符数快速筛掉超限载荷（避免为 20 MiB+ 的图片分配解码缓冲），
    // 解码后再按真实字节数复核：与文件路径共用 [`MAX_IMAGE_BYTES`] 上限。
    let max_payload_chars = MAX_IMAGE_BYTES.div_ceil(3) as usize * 4;
    if payload.len() > max_payload_chars {
        return Err("too-large");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "base64")?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("too-large");
    }
    decode_image(bytes)
}

/// 后台解码（overlay 与内联图片共用）：header 尺寸校验（防恶意 header 撑爆
/// 内存）→ 整图解码 → 解码后复核尺寸（纵深防御）。返回内部原因分类
/// （"format"/"too-large"），不泄漏路径细节。
pub(crate) fn decode_image(buf: Vec<u8>) -> Result<DynamicImage, &'static str> {
    let reader = image::ImageReader::new(std::io::Cursor::new(&buf))
        .with_guessed_format()
        .map_err(|_| "format")?;
    let (w, h) = reader.into_dimensions().map_err(|_| "format")?;
    if u64::from(w) * u64::from(h) > MAX_TOTAL_PIXELS || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        return Err("too-large");
    }
    // 二次构造 Reader（`into_dimensions` 消耗 self）；仍从同一缓冲读取。
    let img = image::ImageReader::new(std::io::Cursor::new(&buf))
        .with_guessed_format()
        .map_err(|_| "format")?
        .decode()
        .map_err(|_| "format")?;
    let (w, h) = img.dimensions();
    if u64::from(w) * u64::from(h) > MAX_TOTAL_PIXELS || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        return Err("too-large");
    }
    Ok(img)
}

fn safety_reason(error: ImageSafetyError) -> &'static str {
    match error {
        ImageSafetyError::NotRegularFile => "not-file",
        ImageSafetyError::BadExtension => "extension",
        ImageSafetyError::MimeMismatch => "format",
        ImageSafetyError::TooLarge(_) => "too-large",
        ImageSafetyError::Io(_) => "io",
        ImageSafetyError::Decode(_) => "format",
    }
}

#[cfg(test)]
#[path = "cache_test.rs"]
mod tests;

/// 单测等待后台线程完成（最多 `timeout`）；返回是否 Ready。
#[cfg(test)]
pub(crate) fn wait_ready(
    source: ImageSource<'_>,
    budget: Budget,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut guard = cache().lock();
        let Some(cell_px) = guard.observed_cell_px else {
            return false;
        };
        let epoch = guard.upload_epoch;
        let key = source_ref(&source, epoch, budget, cell_px);
        match guard.lookup(key) {
            Some(ImageState::Ready(_)) => return true,
            Some(ImageState::Failed(_)) => return false,
            Some(ImageState::Pending) | None => {}
        }
        drop(guard);
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 单测钩子：该来源在当前预算键上是否已有缓存条目（Pending / Ready / Failed）。
///
/// 用于验证「下一帧重新 request 已登记」：登记发生在 `begin` 同步阶段，后台编码
/// 可能已完成（Ready）甚至失败，故不锁定瞬态 Pending。条目缺失说明 effect 未
/// 重试或 deps 缺 `revision()`，与编码速度无关。
#[cfg(test)]
pub(crate) fn has_registered(source: ImageSource<'_>, budget: Budget) -> bool {
    let mut guard = cache().lock();
    let Some(cell_px) = guard.observed_cell_px else {
        return false;
    };
    let epoch = guard.upload_epoch;
    let source = source_ref(&source, epoch, budget, cell_px);
    guard.lookup(source).is_some()
}

/// 单测钩子：下一次请求 `Deferred` 后立即 bump revision（模拟槽位在同一帧内释放）。
///
/// 持久组件树（同一次 `render_frames(frames > 1)`）的下一帧据此观察 deps 变化，
/// 只有 effect 依赖 `revision()` 时才会重试——用于锁定该依赖。
#[cfg(test)]
pub(crate) fn defer_once_and_signal_release_for_test() {
    let mut guard = cache().lock();
    guard.defer_requests = 1;
    guard.signal_release_after_defer = true;
}

/// 单测请求并等待就绪：并发已满被暂缓时重试请求。
/// 生产由可见 slot 的 effect 在 revision 变化时重跑，测试没有该循环。
/// 固定用 Kitty 能力位（不依赖真实终端单元格尺寸，路径可完全离线复现）。
#[cfg(test)]
pub(crate) fn request_until_ready(
    source: ImageSource<'_>,
    budget: Budget,
    timeout: std::time::Duration,
) -> bool {
    let caps = TerminalCaps {
        graphics: crate::kit::terminal_caps::GraphicsProtocol::Kitty,
        ..TerminalCaps::default()
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        request(source, budget, &caps, Some((10, 20)));
        if wait_ready(source, budget, std::time::Duration::from_millis(25)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
    }
}
