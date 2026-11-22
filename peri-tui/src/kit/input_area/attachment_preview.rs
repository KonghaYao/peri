//! composer 上方的待发送图片预览（真实终端图片协议）。
//!
//! 数据流：渲染 body 只做「取最后一张图片附件 → 查缓存 → 写一帧预留几何」，
//! base64 解码、尺寸校验（[`crate::kit::image_overlay::decode_image`]）与协议
//! 编码（[`InlineImage::encode`]）全部由 effect 启动的后台线程完成；完成时
//! bump `RENDER_HEARTBEAT` 唤醒 InputArea 重渲染（与消息区内联图片同一机制）。
//!
//! 门控：`InlineImageEncoder::for_caps` 返回 `None`（无协议 / ITerm2 / 缺单元格
//! 尺寸的 Sixel）时**不发起任何请求**，composer 保持 `[Image #n]` 文本标签，
//! 不做半块字符模拟。缓存只保留最近一张：附件一变就换键，旧编码结果不会画到
//! 新附件上；附件清空（移除/提交）后槽位一并清空，重新粘贴可重试。

use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use parking_lot::Mutex;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::{Constraint, Rect};

use crate::kit::atoms::RENDER_HEARTBEAT;
use crate::kit::atoms::{PENDING_ATTACHMENTS, PendingAttachment};
use crate::kit::image_preview::protocol_available;
use crate::kit::image_preview::{InlineImage, InlineImageEncoder};
use crate::kit::terminal_caps::TerminalCaps;

/// 预览区固定行数（宽高比由 [`InlineImage::encode`] 等比适配，小图不放大）。
pub(super) const PREVIEW_ROWS: u16 = 6;

/// 预览编码的目标列宽上限：composer 再宽也只按 40 列编码，避免为
/// 一张缩略图编码出整屏宽度的协议串。
const MAX_PREVIEW_COLS: u16 = 40;

/// 低于该列宽不再预览：6 行高、几列宽的图既不可辨，也不值得起编码线程。
const MIN_PREVIEW_COLS: u16 = 8;

/// 最近一张图片附件（多图只预览最后一张；底部 `[Image #n]` 标签仍列出全部）。
pub(super) fn target(attachments: &[PendingAttachment]) -> Option<&PendingAttachment> {
    attachments
        .iter()
        .rev()
        .find(|attachment| attachment.media_type.starts_with("image/"))
}

/// 预览编码列宽：取 composer 内容宽度（两侧各留 1 列），窄屏返回 0（不预览）。
pub(super) fn preview_cols(composer_width: u16) -> u16 {
    let cols = composer_width.saturating_sub(2).min(MAX_PREVIEW_COLS);
    if cols < MIN_PREVIEW_COLS { 0 } else { cols }
}

/// 预留高度：预算不足时整块让位（返回 0），保证 transcript 与 composer 可用。
///
/// `reserved` 是 composer + 弹出层已占用行数；预览不与 composer 争预算。
pub(super) fn layout_height(budget: Option<u16>, reserved: u16, wanted: bool) -> u16 {
    if !wanted {
        return 0;
    }
    match budget {
        Some(budget) if reserved.saturating_add(PREVIEW_ROWS) > budget => 0,
        _ => PREVIEW_ROWS,
    }
}

/// 预留区矩形。
///
/// `composer_area` 是 `AreaTracker` 记录的**整块输入区**（不是 composer 本体）：
/// composer 顶部 = 输入区顶 + 弹出层高度 + 队列高度，预览紧贴其上方——与
/// element! 中「队列与 composer 之间插入 View」的顺序一致。首帧（或高度变化
/// 当帧）没有可用的 composer_area 时返回 `None`，由绘制钩子用当帧区域回退。
pub(super) fn preview_rect(
    composer_area: Option<Rect>,
    y_offset: u16,
    height: u16,
) -> Option<Rect> {
    if height == 0 {
        return None;
    }
    let area = composer_area?;
    let y = area.y.saturating_add(y_offset);
    if y.saturating_add(height) > area.bottom() {
        return None;
    }
    Some(Rect::new(area.x, y, area.width, height))
}

// ── 缓存键 ────────────────────────────────────────────────────────────────

/// 预览身份：base64 内容哈希 + media_type + 目标单元格尺寸。
///
/// 内容哈希用 FNV-1a（进程内身份，不落盘不跨进程），避免为几 MB 载荷引入
/// 新依赖；哈希只在附件变化时计算一次（见 [`PreviewKeyMemo`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreviewKey {
    digest: u64,
    media_type: String,
    cols: u16,
    rows: u16,
}

impl PreviewKey {
    fn new(attachment: &PendingAttachment, cols: u16, rows: u16) -> Self {
        Self {
            digest: content_digest(&attachment.base64_data),
            media_type: attachment.media_type.clone(),
            cols,
            rows,
        }
    }
}

fn content_digest(base64_data: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in base64_data.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 同一附件只算一次内容哈希。
///
/// base64 载荷上限 20MiB → ~27MiB 文本，每帧全量哈希是纯浪费：
/// `PendingAttachment` 在 composer 内不可变，`(数量, media_type, 长度, 缓冲地址,
/// 目标尺寸)` 足以识别「同一个附件」。附件清空时指纹一并清空——重新粘贴
/// （哪怕内容相同）会重新哈希并允许重试。
#[derive(Default)]
pub(super) struct PreviewKeyMemo {
    fingerprint: Option<Fingerprint>,
    key: Option<Arc<PreviewKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    count: usize,
    media_type: String,
    len: usize,
    ptr: usize,
    cols: u16,
    rows: u16,
}

impl Hook for PreviewKeyMemo {}

impl PreviewKeyMemo {
    pub(super) fn key_for(
        &mut self,
        target: Option<&PendingAttachment>,
        count: usize,
        cols: u16,
        rows: u16,
    ) -> Option<Arc<PreviewKey>> {
        let target = target.filter(|_| cols > 0 && rows > 0);
        let fingerprint = target.map(|attachment| Fingerprint {
            count,
            media_type: attachment.media_type.clone(),
            len: attachment.base64_data.len(),
            ptr: attachment.base64_data.as_ptr() as usize,
            cols,
            rows,
        });
        if self.fingerprint != fingerprint {
            self.fingerprint = fingerprint;
            self.key = target.map(|attachment| Arc::new(PreviewKey::new(attachment, cols, rows)));
        }
        self.key.clone()
    }
}

// ── 三态缓存 ──────────────────────────────────────────────────────────────

/// 三态：渲染 body 只克隆 [`Arc`]，不做 IO。
#[derive(Clone)]
pub(super) enum PreviewState {
    Pending,
    Ready(Arc<InlineImage>),
    /// 失败原因只用于诊断与单测；渲染统一回落文本标签，不向用户展示。
    Failed(&'static str),
}

impl std::fmt::Debug for PreviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Ready(image) => f.debug_tuple("Ready").field(image).finish(),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

/// 只保留最近一个请求的槽位：同一时刻 composer 只显示一张预览。
#[derive(Default)]
struct Cache {
    key: Option<PreviewKey>,
    state: Option<PreviewState>,
    inflight: Option<u64>,
    next_request: u64,
    revision: u64,
}

impl Cache {
    fn state(&self, key: &PreviewKey) -> Option<PreviewState> {
        (self.key.as_ref() == Some(key))
            .then(|| self.state.clone())
            .flatten()
    }

    /// 登记一次请求；同键已有条目（含 Pending/Failed）时返回 `None`（去重）。
    fn begin(&mut self, key: PreviewKey) -> Option<u64> {
        if self.key.as_ref() == Some(&key) && self.state.is_some() {
            return None;
        }
        self.next_request = self.next_request.wrapping_add(1);
        self.key = Some(key);
        self.state = Some(PreviewState::Pending);
        self.inflight = Some(self.next_request);
        Some(self.next_request)
    }

    /// 写回后台结果；请求代数或键不匹配（被新附件取代）时丢弃。返回是否更新。
    fn complete(
        &mut self,
        key: &PreviewKey,
        id: u64,
        result: Result<Arc<InlineImage>, &'static str>,
    ) -> bool {
        if self.inflight != Some(id) || self.key.as_ref() != Some(key) {
            return false;
        }
        self.inflight = None;
        self.state = Some(match result {
            Ok(image) => PreviewState::Ready(image),
            Err(reason) => PreviewState::Failed(reason),
        });
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn clear(&mut self) {
        self.key = None;
        self.state = None;
        self.inflight = None;
    }
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// 纯查表：Ready 返回协议句柄，Pending/Failed/无条目返回 `None`。
pub(super) fn lookup_ready(key: &PreviewKey) -> Option<Arc<InlineImage>> {
    match cache().lock().state(key) {
        Some(PreviewState::Ready(image)) => Some(image),
        _ => None,
    }
}

/// 当前附件编码是否失败：失败不预留高度，避免永久空白条。
pub(super) fn failed(key: &PreviewKey) -> bool {
    matches!(cache().lock().state(key), Some(PreviewState::Failed(_)))
}

/// effect 边界请求：`target`/`key` 为 `None` 时清空槽位（附件移除/提交）。
///
/// 同键重复调用走缓存去重，不重复解码；后台线程只持有 base64 副本与编码器。
pub(super) fn request(
    target: Option<&PendingAttachment>,
    key: Option<&PreviewKey>,
    cols: u16,
    rows: u16,
    caps: &TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    let (Some(target), Some(key)) = (target, key) else {
        cache().lock().clear();
        return;
    };
    // 协议不可用时在构造 Picker 之前返回（`Picker::halfblocks` 有 tmux 副作用）。
    let Some(encoder) = InlineImageEncoder::for_caps(caps, cell_px) else {
        cache().lock().clear();
        return;
    };
    let Some(id) = cache().lock().begin(key.clone()) else {
        return;
    };
    let base64_data = target.base64_data.clone();
    let thread_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("peri-attachment-preview".to_string())
        .spawn(move || {
            // 解码/编码 panic 也要回收 inflight 槽位，否则同键请求会永久停在 Pending。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encode(&encoder, &base64_data, cols, rows)
            }))
            .unwrap_or(Err("panic"));
            finish(thread_key, id, result);
        });
    if spawned.is_err() {
        finish(key.clone(), id, Err("spawn"));
    }
}

fn encode(
    encoder: &InlineImageEncoder,
    base64_data: &str,
    cols: u16,
    rows: u16,
) -> Result<Arc<InlineImage>, &'static str> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|_| "base64")?;
    let image = crate::kit::image_overlay::decode_image(bytes)?;
    encoder
        .encode(image, cols, rows)
        .map(Arc::new)
        .ok_or("encode")
}

fn finish(key: PreviewKey, id: u64, result: Result<Arc<InlineImage>, &'static str>) {
    // 状态与唤醒在同一把锁内发布：否则重渲染可能先看到 Ready、再读到旧版本。
    let updated = cache().lock().complete(&key, id, result);
    if updated {
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }
}

/// 单测：缓存版本号（只在编码完成并发布时递增）。
#[cfg(test)]
pub(super) fn revision() -> u64 {
    cache().lock().revision
}

/// 单测：读取当前槽位状态。
#[cfg(test)]
pub(super) fn state_of(key: &PreviewKey) -> Option<PreviewState> {
    cache().lock().state(key)
}

// ── 一帧几何 + 绘制钩子 ───────────────────────────────────────────────────

/// 渲染 body 的输入：全部是当帧已知值（附件、宽度、能力、预算）。
pub(super) struct PreviewInputs<'a> {
    pub(super) attachments: &'a [PendingAttachment],
    pub(super) composer_width: u16,
    pub(super) caps: TerminalCaps,
    pub(super) budget: Option<u16>,
    /// composer + 弹出层已占用行数（预览只在这之后取预算）。
    pub(super) reserved: u16,
    /// 是否允许显示（`hidden` / h<8 降级时为 false）。
    pub(super) visible: bool,
}

/// 一帧的预览结果（预留高度 + 缓存键）：占位高度与编码请求都以它为准。
pub(super) struct PreviewPlan {
    pub(super) height: u16,
    key: Option<Arc<PreviewKey>>,
}

/// InputArea 的预览入口：解析缓存键、按预算定高度、注册编码 effect。
///
/// 只在这里调用 `hooks.use_hook`/`use_effect`，保证 hook 顺序与组件一致。
pub(super) fn use_preview(hooks: &mut Hooks<'_, '_>, inputs: PreviewInputs<'_>) -> PreviewPlan {
    let cols = preview_cols(inputs.composer_width);
    let (key, is_failed) = {
        let memo = hooks.use_hook(PreviewKeyMemo::default);
        let key = memo.key_for(
            target(inputs.attachments),
            inputs.attachments.len(),
            cols,
            PREVIEW_ROWS,
        );
        let is_failed = key.as_ref().is_some_and(|key| failed(key));
        (key, is_failed)
    };
    // 单元格像素尺寸只在真的有图片附件时才探测（`window_size` 是 ioctl）。
    let cell_px = key
        .as_ref()
        .and_then(|_| crate::kit::image_preview::cell_pixel_size());
    let protocol = key
        .as_ref()
        .is_some_and(|_| protocol_available(&inputs.caps, cell_px));
    let height = layout_height(
        inputs.budget,
        inputs.reserved,
        protocol && !is_failed && inputs.visible,
    );
    let caps = inputs.caps;
    let request_key = key.clone();
    hooks.use_effect(
        move || {
            let atom = PENDING_ATTACHMENTS.state();
            let attachments = atom.read();
            request(
                target(&attachments),
                request_key.as_deref(),
                cols,
                PREVIEW_ROWS,
                &caps,
                cell_px,
            );
        },
        (key.clone(), cols),
    );
    PreviewPlan { height, key }
}

/// 每帧写预留几何（钩子只读；`write_no_update` 避免渲染自激）。
pub(super) fn write_frame(
    frame: State<PreviewFrame>,
    plan: &PreviewPlan,
    y_offset: u16,
    composer_area: Option<Rect>,
) {
    let rect = preview_rect(composer_area, y_offset, plan.height);
    let mut frame = frame.write_no_update();
    frame.update(plan.height, y_offset, rect, plan.key.clone());
}

/// 预览占位 View：只占高度，不渲染内容（像素由 [`AttachmentPreviewDrawHook`] 写）。
pub(super) fn preview_spacer(height: u16) -> AnyElement<'static> {
    element!(
        View(
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        )
    )
    .into_any()
}

/// 渲染 body 每帧写入、绘制钩子只读的一帧信息。
#[derive(Default)]
pub(super) struct PreviewFrame {
    /// 预留行数（0 = 无预览）。
    height: u16,
    /// 预留区相对输入区顶部的行偏移（弹出层 + 队列高度）。
    y_offset: u16,
    /// 由 composer_area 预计算的屏幕矩形；`None` 时钩子用当帧输入区回退。
    rect: Option<Rect>,
    key: Option<Arc<PreviewKey>>,
}

impl PreviewFrame {
    pub(super) fn update(
        &mut self,
        height: u16,
        y_offset: u16,
        rect: Option<Rect>,
        key: Option<Arc<PreviewKey>>,
    ) {
        self.height = height;
        self.y_offset = y_offset;
        self.rect = rect;
        self.key = key;
    }

    /// 在输入区当帧区域 `area` 内绘制 Ready 的预览；其余状态 no-op。
    fn draw(&self, buf: &mut Buffer, area: Rect) {
        if self.height == 0 {
            return;
        }
        let Some(key) = self.key.as_ref() else {
            return;
        };
        let rect = self.rect.unwrap_or_else(|| {
            Rect::new(
                area.x,
                area.y.saturating_add(self.y_offset),
                area.width,
                self.height,
            )
        });
        if rect.y < area.y || rect.bottom() > area.bottom() || rect.right() > area.right() {
            return;
        }
        let Some(image) = lookup_ready(key) else {
            return;
        };
        image.render(rect, buf);
    }
}

/// `post_component_draw` 时把 Ready 的内联图片画到预留区；其余状态 no-op。
///
/// 绘制放在 composer 之后（本钩子注册在 `CjkGhostFix` 之后），占位符不会被
/// 边线修复覆盖；面积必须完整落在输入区内，宁可空一帧也不画到组件外。
pub(super) struct AttachmentPreviewDrawHook {
    frame: State<PreviewFrame>,
}

impl AttachmentPreviewDrawHook {
    pub(super) fn new(frame: State<PreviewFrame>) -> Self {
        Self { frame }
    }
}

impl Hook for AttachmentPreviewDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let area = drawer.area;
        let frame = self.frame.read();
        frame.draw(drawer.buffer_mut(), area);
    }
}

#[cfg(test)]
#[path = "attachment_preview_test.rs"]
mod tests;
