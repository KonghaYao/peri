//! 消息区内联图片：终端图片协议（Kitty/Sixel）像素渲染 + 后台解码编码。
//!
//! 渲染 body 只做纯查表（[`lookup_ready`]），不读文件、不起线程（TUI-RENDER-001）；
//! 请求由消息区 effect 在事件边界发起（[`request`]），后台线程复用
//! [`crate::kit::image_safety::read_validated_image`] 与
//! [`crate::kit::image_overlay::decode_image`] 完成校验与解码，再经
//! [`crate::kit::image_preview::InlineImage`] 一次性 resize+encode。
//!
//! 缓存键为 `(canonical path, mtime, len, cols, rows)`，只保留编码后的协议
//! （不保留 `DynamicImage`），按 LRU 与 64MiB 估算字节上限淘汰。远程 URL 与
//! 危险 scheme 不解析路径、不请求；终端不支持协议（None/ITerm2/无单元格尺寸
//! 的 Sixel）时请求直接返回，渲染保持 `[Image: alt] (url)` 文本降级。失败同样
//! 只降级为文本行，原因不进入渲染通知。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

#[cfg(test)]
use std::time::Duration;

use parking_lot::Mutex;

use crate::kit::atoms::RENDER_HEARTBEAT;
use crate::kit::atoms::TERMINAL_CAPS;
use crate::kit::image_preview;
use crate::kit::image_preview::InlineImage;
use crate::kit::image_safety::{ImageSafetyError, MAX_CACHE_BYTES, read_validated_image};
use crate::kit::terminal_caps::TerminalCaps;
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_tui_components::markdown::{UrlKind, classify_url, image_links};

/// 消息区图片的最大显示行数（正文图）。
pub(crate) const MAX_ROWS: u16 = 8;

/// 用户气泡图片行预算；与 `render/user.rs::USER_BODY_MAX_LINES` 对齐（MVP
/// 用户正文上限 6 行）。
pub(crate) const USER_IMAGE_ROWS: u16 = 6;

/// 内联图片缓存总字节上限（复用 T5 的累计解码缓存上限常量）。
const CACHE_BYTES: usize = MAX_CACHE_BYTES as usize;

/// Bound metadata even for zero-byte failures and multiple URLs for the same file.
const MAX_CACHE_ENTRIES: usize = 512;
const MAX_SOURCE_ALIASES: usize = 1024;

/// 同时解码的后台线程上限；超出的请求留待下次 effect（revision 变化）重试。
/// 单个可见消息可能含多张图片，无上限会按图片数起线程并各自持有解码缓冲。
const MAX_CONCURRENT: usize = 4;

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
    /// 新状态已写入缓存。
    Updated,
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

/// 三态缓存值：render body 只克隆 [`Arc`]，不阻塞。
#[derive(Clone)]
pub(crate) enum InlineImageState {
    Pending,
    Ready(Arc<InlineImage>),
    /// 失败原因只用于诊断与单测；渲染统一回落降级行，不向用户展示。
    #[allow(dead_code)]
    Failed(&'static str),
}

impl std::fmt::Debug for InlineImageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Ready(image) => f.debug_tuple("Ready").field(image).finish(),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

/// 渲染期可用的内联图片：句柄 + 真实占用单元格。
#[derive(Debug, Clone)]
pub(crate) struct ReadyImage {
    pub(crate) image: Arc<InlineImage>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

/// 源身份（渲染侧与调度侧共用的纯文本键，不含 IO）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceKey {
    source: String,
    cols: u16,
    rows: u16,
}

/// 版本键：同一路径的 mtime/len 变化视为不同图片。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime: u128,
    len: u64,
    cols: u16,
    rows: u16,
}

struct Alias {
    key: CacheKey,
    last_used: u64,
}

struct Entry {
    state: InlineImageState,
    last_used: u64,
    bytes: usize,
}

/// 单进程缓存本体。独立成结构体便于单测注入键与预算。
#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, Entry>,
    aliases: HashMap<SourceKey, Alias>,
    inflight: HashMap<CacheKey, u64>,
    next_request: u64,
    clock: u64,
    bytes: usize,
    revision: u64,
}

impl Cache {
    fn lookup(&mut self, source: &SourceKey) -> Option<InlineImageState> {
        let alias = self.aliases.get_mut(source)?;
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        alias.last_used = now;
        let Some(entry) = self.entries.get_mut(&alias.key) else {
            self.aliases.remove(source);
            return None;
        };
        entry.last_used = now;
        Some(entry.state.clone())
    }

    /// 登记一次请求；`Spawn` 表示调用方应 spawn 后台线程。
    /// 同源同尺寸已有条目（含 Pending/Failed）时只更新别名与 LRU，不重复请求。
    fn begin(&mut self, source: SourceKey, key: CacheKey) -> BeginOutcome {
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        if !self.entries.contains_key(&key) && self.inflight.len() >= MAX_CONCURRENT {
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
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = now;
            return BeginOutcome::Existing;
        }
        self.next_request = self.next_request.wrapping_add(1);
        let id = self.next_request;
        self.entries.insert(
            key.clone(),
            Entry {
                state: InlineImageState::Pending,
                last_used: now,
                bytes: 0,
            },
        );
        self.inflight.insert(key, id);
        self.evict_to_budget(CACHE_BYTES);
        BeginOutcome::Spawn(id)
    }

    /// 写回后台结果；请求代数不匹配（被更新请求取代）或别名已指向别的版本
    /// 时丢弃。返回槽位与缓存的实际变化。
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
        let (state, bytes) = match result {
            Ok(image) => {
                let bytes = image.retained_bytes();
                (InlineImageState::Ready(image), bytes)
            }
            Err(reason) => (InlineImageState::Failed(reason), 0),
        };
        entry.state = state;
        entry.bytes = bytes;
        self.clock = self.clock.wrapping_add(1);
        entry.last_used = self.clock;
        self.bytes = self.bytes.saturating_add(bytes);
        self.revision = self.revision.wrapping_add(1);
        self.evict_to_budget(CACHE_BYTES);
        CompleteOutcome::Updated
    }

    fn evict_to_budget(&mut self, budget: usize) {
        while self.bytes > budget || self.entries.len() > MAX_CACHE_ENTRIES {
            let victim = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !matches!(entry.state, InlineImageState::Pending)
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

/// 纯查表：命中 Ready 返回协议句柄与真实单元格，Pending/Failed/远程/未知一律
/// `None`。调用方（render body）不得在此函数外做任何文件 IO。
#[must_use]
pub(crate) fn lookup_ready(source: &str, cols: u16, rows: u16) -> Option<ReadyImage> {
    let key = source_key(source, cols, rows)?;
    let state = cache().lock().lookup(&key)?;
    match state {
        InlineImageState::Ready(image) => Some(ReadyImage {
            cols: image.cols(),
            rows: image.rows(),
            image,
        }),
        InlineImageState::Pending | InlineImageState::Failed(_) => None,
    }
}

/// 非阻塞请求：本地路径经校验后交给后台线程解码并编码协议；重复请求由缓存去重。
pub(crate) fn request(source: &str, cols: u16, rows: u16) {
    let caps = *TERMINAL_CAPS.state().read();
    let cell_px = image_preview::cell_pixel_size();
    request_with(source, cols, rows, &caps, cell_px);
}

/// [`request`] 的显式能力注入版：测试用它直接指定协议，不读全局能力位。
///
/// 不支持协议时不构造 `Picker`（`Picker::halfblocks` 在 tmux 下有 spawn 副作用），
/// 也不登记缓存——渲染侧继续走文本降级。
pub(crate) fn request_with(
    source: &str,
    cols: u16,
    rows: u16,
    caps: &TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    if cols == 0 || rows == 0 {
        return;
    }
    let Some(encoder) = image_preview::InlineImageEncoder::for_caps(caps, cell_px) else {
        return;
    };
    let Some(source_key) = source_key(source, cols, rows) else {
        return;
    };
    let Some(path) = resolve_local_path(source) else {
        return;
    };
    let (path, mtime, len) = file_version(path);
    let key = CacheKey {
        path: path.clone(),
        mtime,
        len,
        cols,
        rows,
    };
    let id = match cache().lock().begin(source_key, key.clone()) {
        BeginOutcome::Spawn(id) => id,
        BeginOutcome::Existing | BeginOutcome::Deferred => return,
    };
    let thread_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("peri-inline-image".to_string())
        .spawn(move || {
            // 解码/编码 panic 也要回收 inflight 槽位，否则并发上限会被永久占满。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encode_validated(&path, &encoder, cols, rows)
            }))
            .unwrap_or(Err("panic"));
            finish(thread_key, id, result);
        });
    if spawned.is_err() {
        finish(key, id, Err("spawn"));
    }
}

/// 扫描一条消息中的本地图片并发起请求（effect 边界调用，非 render body）。
///
/// assistant 走完整 markdown 扫描；用户气泡只识别「整行仅 `![alt](path)`」，
/// 与渲染侧的判定共用 [`standalone_image_url`]。
pub(crate) fn request_vm_images(vm: &TuiRenderUnit, content_cols: u16, process_cols: u16) {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            let cols = if bubble.is_brief() {
                process_cols
            } else {
                content_cols
            };
            // 只请求渲染器可能替换的独占行图片（整行仅图片语法与空白）。
            // 同段多图也在此覆盖；行级判定是段落级 standalone 的超集。
            for line in bubble.text.lines() {
                for url in image_line_urls(line) {
                    request(&url, cols, MAX_ROWS);
                }
            }
        }
        TuiRenderUnit::TuiUserBubble(bubble) => {
            for line in bubble.text.lines() {
                if let Some(url) = standalone_image_url(line) {
                    request(&url, content_cols, USER_IMAGE_ROWS);
                }
            }
        }
        _ => {}
    }
}

/// 该 VM 是否可能含图片语法；仅用于缓存失效门控（可能含代码块内的假阳性）。
#[must_use]
pub(crate) fn vm_may_have_image(vm: &TuiRenderUnit) -> bool {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.text.contains("!["),
        TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.contains("!["),
        _ => false,
    }
}

/// 整行仅一张图片语法时返回 url；行内混排、多个语法或空 url 返回 `None`。
#[must_use]
pub(crate) fn standalone_image_url(line: &str) -> Option<String> {
    let mut urls = image_line_urls(line);
    (urls.len() == 1).then(|| urls.remove(0))
}

/// 整行仅图片语法与空白时返回该行所有图片 url；行内混排返回空。
///
/// 行级判定是段落级 standalone 的超集（段落还受其它行文本影响），
/// 用于请求调度时宁可多请求，也不漏掉独占段落的图片。
fn image_line_urls(line: &str) -> Vec<String> {
    if !line.contains("![") {
        return Vec::new();
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let links = image_links(trimmed);
    if links.is_empty() {
        return Vec::new();
    }
    let mut gaps = String::new();
    let mut cursor = 0;
    for link in &links {
        if link.byte_start < cursor || link.byte_end > trimmed.len() {
            return Vec::new();
        }
        gaps.push_str(&trimmed[cursor..link.byte_start]);
        cursor = link.byte_end;
    }
    gaps.push_str(&trimmed[cursor..]);
    if !gaps.trim().is_empty() {
        return Vec::new();
    }
    links
        .into_iter()
        .map(|link| link.url.trim().to_string())
        .filter(|url| !url.is_empty())
        .collect()
}

fn source_key(source: &str, cols: u16, rows: u16) -> Option<SourceKey> {
    let source = source.trim();
    if source.is_empty() || classify_url(source) != UrlKind::Local {
        return None;
    }
    Some(SourceKey {
        source: source.to_string(),
        cols,
        rows,
    })
}

/// 本地 URL/路径解析：`file://` 去 scheme，相对路径按进程 cwd 拼接。
/// 不做 canonicalize（请求线程内的读取会以 fd 为准）。
fn resolve_local_path(source: &str) -> Option<PathBuf> {
    let source = source.trim();
    if source.is_empty() || classify_url(source) != UrlKind::Local {
        return None;
    }
    let raw = source.strip_prefix("file://").unwrap_or(source);
    let decoded = percent_decode(raw);
    if decoded.is_empty() {
        return None;
    }
    let path = PathBuf::from(decoded);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn file_version(path: PathBuf) -> (PathBuf, u128, u64) {
    let canonical = path.canonicalize().unwrap_or(path);
    match std::fs::metadata(&canonical) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            (canonical, mtime, meta.len())
        }
        Err(_) => (canonical, 0, 0),
    }
}

fn finish(key: CacheKey, id: u64, result: Result<Arc<InlineImage>, &'static str>) {
    // 版本号与 Ready 状态必须在同一把锁内发布：否则 `wait_ready` 可能先看到
    // 完成态，再读到旧的全局版本，导致渲染缓存判为未变化。丢弃结果也释放了
    // 槽位，同样要唤醒 effect，让并发暂缓的请求重新排队。
    let mut guard = cache().lock();
    if guard.complete(&key, id, result).wakes_render() {
        REVISION.fetch_add(1, Ordering::SeqCst);
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
}

fn encode_validated(
    path: &Path,
    encoder: &image_preview::InlineImageEncoder,
    cols: u16,
    rows: u16,
) -> Result<Arc<InlineImage>, &'static str> {
    let (_meta, buffer) = read_validated_image(path).map_err(safety_reason)?;
    let image = crate::kit::image_overlay::decode_image(buffer)?;
    encoder
        .encode(image, cols, rows)
        .map(Arc::new)
        .ok_or("encode")
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

/// 单测等待后台线程完成（最多 `timeout`）；返回是否 Ready。
#[cfg(test)]
pub(crate) fn wait_ready(source: &str, cols: u16, rows: u16, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let Some(key) = source_key(source, cols, rows) else {
            return false;
        };
        let state = cache().lock().lookup(&key);
        match state {
            Some(InlineImageState::Ready(_)) => return true,
            Some(InlineImageState::Failed(_)) => return false,
            Some(InlineImageState::Pending) | None => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 单测请求并等待就绪：并发已满被暂缓时重试请求。
/// 生产由可见 slot 的 effect 在 revision 变化时重跑，测试没有该循环。
/// 固定用 Kitty 能力位（不依赖真实终端单元格尺寸，路径可完全离线复现）。
#[cfg(test)]
pub(crate) fn request_until_ready(source: &str, cols: u16, rows: u16, timeout: Duration) -> bool {
    let caps = TerminalCaps {
        graphics: crate::kit::terminal_caps::GraphicsProtocol::Kitty,
        ..TerminalCaps::default()
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        request_with(source, cols, rows, &caps, None);
        if wait_ready(source, cols, rows, Duration::from_millis(25)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
    }
}

#[cfg(test)]
#[path = "inline_image_test.rs"]
mod tests;
