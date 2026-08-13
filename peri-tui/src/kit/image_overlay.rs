//! 上下文 overlay 图片预览（image-p0-p1-spec §7 T7）。
//!
//! 在 @image 行（消息区）/ 输入区光标所在 `@image` 行 / 焦点 entry 上触发
//! 像素预览浮层：居中 + 边框 + meta 行（`[Image: 文件名 · WxH · 大小 · MIME]`），
//! 像素区经 T6 窄接口（[`crate::kit::image_preview`]）渲染（Kitty 协议，占位符
//! 从 buffer 消失即自动清理，S2 §7）。
//!
//! 三态触发仲裁（§7.3）：优先级 **hover > cursor > focus**；任一来源清空 →
//! `Idle`（隐藏清理）。遮挡（`mouse_router::is_occluded`）→ `Idle`（§7.5）。
//!
//! 安全（§6.1 Q6 / §5.7）：仅受管理目录（`~/.peri/images`，T5 分级）自动
//! 像素预览；手工路径 `Degraded` 文本降级（提示文案）；T5 全链校验失败 →
//! `Error`（固定文案，reason 不泄漏路径细节）。TOCTOU（§6.2-3）：校验经
//! [`read_validated_image`] 返回缓冲，解码线程只消费缓冲、不按路径二次打开
//! （F2 评审修复）；解码后复核尺寸（纵深防御）。
//!
//! 状态机写入边界：事件/effect（`request_preview`）与后台解码线程；渲染
//! body 只读（TUI-RENDER-001）。AsyncImage 生命周期与绘制在
//! [`OverlayDrawHook`]（跨帧持久，post_component_draw 在 Positioned clear
//! 之后绘制）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::Poll;

use crate::i18n;
use crate::kit::atoms::{
    FOCUSED_ENTRY, FocusedEntry, IMAGE_HOVER, IMAGE_PREVIEW_STATE, INPUT_SNAPSHOT,
    ImagePreviewState, InputSnapshot, RENDER_HEARTBEAT,
};
use crate::kit::image_preview::{AsyncImage, picker_for, supported as preview_supported};
use crate::kit::image_safety::{
    PathGrade, grade_path, read_validated_image, sanitize_for_terminal,
};
use crate::kit::message_area::render::parse_image_line;
use crate::kit::mouse_router;
use crate::kit::tui_render_unit::TuiRenderUnit;
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use image::DynamicImage;
use image::GenericImageView;
use peri_theme::atoms::THEME_ATOM;
use ratatui_image::picker::Picker;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::widgets::Block;
use ratatui_kit::ratatui::widgets::BorderType;
use ratatui_kit::ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr;

/// 像素区最小终端尺寸（§7.4）：终端小于该尺寸仅显示 meta 行（纯文本降级）。
const MIN_PREVIEW_TERM_W: u16 = 30;
const MIN_PREVIEW_TERM_H: u16 = 12;

/// 预览请求 id——切换时旧解码线程的陈旧结果据此丢弃（§7.5 竞态防御）。
static PREVIEW_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

/// `Picker` 进程级缓存：`TerminalCaps` 启动时探测一次后不再变化，惰性构造。
/// 测试不走此缓存（直接 `picker_for` 构造），避免全局污染。
static PICKER: OnceLock<Picker> = OnceLock::new();

fn cached_picker() -> &'static Picker {
    PICKER.get_or_init(|| {
        let caps = *crate::kit::atoms::TERMINAL_CAPS.state().read();
        picker_for(&caps)
    })
}

// ── 触发仲裁（§7.3）──────────────────────────────────────────────────────

/// cursor 触发源：光标所在行（按 `\n` 切分 + `cursor_char` 定位）trim 后以
/// `@image ` 开头 → 提取路径（与 T4 `parse_image_line` 同一判定）；其他行/空
/// 文本 → None。
pub(crate) fn cursor_image_path(snapshot: &InputSnapshot) -> Option<String> {
    let mut line_start = 0usize; // 当前行起始 char 索引
    for line in snapshot.text.split('\n') {
        let line_end = line_start + line.chars().count();
        if snapshot.cursor_char <= line_end {
            // 光标在该行内（含行尾位置）；判定只作用于本行，不跨行。
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("@image ") {
                let path = rest.trim();
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
            return None;
        }
        line_start = line_end + 1; // +1 = 换行符
    }
    None
}

/// focus 触发源：`FOCUSED_ENTRY` 指向的 slot 的 `TuiUserBubble` 文本中第一个
/// @image 行（§7.3；与 T4 `ImageLineHit` 同源——都由 `parse_image_line` 判定，
/// 避免判定漂移）。slot 越界 / 非 user bubble / 无 @image 行 → None。
pub(crate) fn focus_image_path(focused: &FocusedEntry) -> Option<String> {
    let vm_handle = crate::kit::atoms::VIEW_MODELS.state();
    let vm_guard = vm_handle.read();
    let data = match vm_guard.items.get(focused.slot) {
        Some(TuiRenderUnit::TuiUserBubble(data)) => data,
        _ => return None,
    };
    data.text.lines().find_map(parse_image_line)
}

/// 三态触发仲裁（§7.3）：优先级 **hover > cursor > focus**；遮挡
/// （`mouse_router::is_occluded`）或全部来源清空 → None（→ Idle，隐藏清理）。
/// 组件渲染 body 每帧调用；结果经 `use_effect` 驱动请求（事件边界）。
pub(crate) fn resolve_preview_target() -> Option<String> {
    if mouse_router::is_occluded() {
        return None;
    }
    if let Some(hover) = IMAGE_HOVER.state().read().as_ref() {
        return Some(hover.path.clone());
    }
    let snapshot = INPUT_SNAPSHOT.state().read().clone();
    if let Some(path) = cursor_image_path(&snapshot) {
        return Some(path);
    }
    if let Some(focused) = FOCUSED_ENTRY.state().read().as_ref()
        && let Some(path) = focus_image_path(focused)
    {
        return Some(path);
    }
    None
}

// ── 几何（§7.4）──────────────────────────────────────────────────────────

/// overlay 几何：居中于终端，宽 = 60% 终端宽 × 高 = 40% 终端高（含边框），
/// 饱和运算（TUI-TEXT-001）。终端过小（宽或高 < 2）→ None（不渲染）。
pub(crate) fn preview_geometry(term_w: u16, term_h: u16) -> Option<Rect> {
    let w = ((term_w as u32) * 6 / 10) as u16;
    let h = ((term_h as u32) * 4 / 10) as u16;
    if w < 2 || h < 2 {
        return None;
    }
    Some(Rect::new(
        term_w.saturating_sub(w) / 2,
        term_h.saturating_sub(h) / 2,
        w,
        h,
    ))
}

// ── 请求流程（§7.3，事件边界调用）────────────────────────────────────────

/// 后台解码（生产路径）：从 T5 已校验缓冲解码（F2：**不得按路径二次打开**——
/// validate 通过后文件可能被替换，缓冲是唯一可信数据源，字节数 ≤
/// [`MAX_IMAGE_BYTES`] 天然生效）。header 尺寸校验（防恶意 header 撑爆内存）
/// → 整图解码 → 解码后复核尺寸（纵深防御）。返回内部原因分类
/// （"io"/"format"/"too-large"），不泄漏路径细节。
fn decode_image(buf: Vec<u8>) -> Result<DynamicImage, &'static str> {
    let reader = image::ImageReader::new(std::io::Cursor::new(&buf))
        .with_guessed_format()
        .map_err(|_| "format")?;
    let (w, h) = reader.into_dimensions().map_err(|_| "format")?;
    if u64::from(w) * u64::from(h) > crate::kit::image_safety::MAX_TOTAL_PIXELS
        || w > crate::kit::image_safety::MAX_IMAGE_SIDE
        || h > crate::kit::image_safety::MAX_IMAGE_SIDE
    {
        return Err("too-large");
    }
    // 二次构造 Reader（`into_dimensions` 消耗 self）；仍从同一缓冲读取。
    let img = image::ImageReader::new(std::io::Cursor::new(&buf))
        .with_guessed_format()
        .map_err(|_| "format")?
        .decode()
        .map_err(|_| "format")?;
    let (w, h) = img.dimensions();
    if u64::from(w) * u64::from(h) > crate::kit::image_safety::MAX_TOTAL_PIXELS
        || w > crate::kit::image_safety::MAX_IMAGE_SIDE
        || h > crate::kit::image_safety::MAX_IMAGE_SIDE
    {
        return Err("too-large");
    }
    Ok(img)
}

/// 请求预览（事件边界调用，非 render body——TUI-RENDER-001）。`path=None`
/// （触发清空/遮挡）→ `Idle`（隐藏清理，§7.5）。
pub(crate) fn request_preview(path: Option<&str>) {
    request_preview_with(path, None, decode_image);
}

/// [`request_preview`] 注入版（单测）：`managed_root` 模拟受管理根
/// （`grade_path_with_root`），`decode` 可注入时序/结果（收到 T5 已校验缓冲，
/// 不接触路径——F2 TOCTOU 契约）。
pub(crate) fn request_preview_with(
    path: Option<&str>,
    managed_root: Option<&Path>,
    decode: impl FnOnce(Vec<u8>) -> Result<DynamicImage, &'static str> + Send + 'static,
) {
    let Some(path) = path else {
        *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Idle;
        return;
    };
    // T5 分级（§6.1 Q6）：canonicalize + 受管理目录判定。
    let (grade, canonical) = match managed_root {
        Some(root) => crate::kit::image_safety::grade_path_with_root(Path::new(path), root),
        None => grade_path(Path::new(path)),
    };
    let Some(canonical) = canonical else {
        *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Error {
            path: path.to_string().into(),
            reason: "invalid-path".to_string(),
        };
        return;
    };
    // 仅受管理目录自动预览；手工路径/其他 → Degraded 文本降级（不触发解码）。
    if grade != PathGrade::Managed {
        *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Degraded {
            path: canonical,
            reason: i18n::tr("image-preview-degraded"),
        };
        return;
    }
    // T5 全链校验（常规文件/扩展名/MIME/字节上限/像素上限）；失败 → Error。
    // 返回校验缓冲（≤ MAX_IMAGE_BYTES）——decode 线程只消费缓冲，不再按
    // 路径二次打开（F2：闭合 validate 与 decode 之间的 TOCTOU 窗口）。
    let (meta, buf) = match read_validated_image(&canonical) {
        Ok(ok) => ok,
        Err(err) => {
            tracing::warn!(error = %err, "image preview: validation failed");
            *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Error {
                path: canonical,
                reason: "validate".to_string(),
            };
            return;
        }
    };
    // 后台解码（std::thread 模型，S2 §2 不引 tokio）；写回前校验请求 id，
    // 陈旧结果丢弃（§7.5 切换竞态）。闭包捕获 canonical 副本（外部后续
    // spawn 失败分支仍要用原值）。
    let id = PREVIEW_REQUEST_ID.fetch_add(1, Ordering::SeqCst) + 1;
    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Loading {
        path: canonical.clone(),
        grade,
    };
    let canonical_for_thread = canonical.clone();
    let spawned = std::thread::Builder::new()
        .name("peri-image-decode".to_string())
        .spawn(move || {
            let result = decode(buf);
            // 请求 id：fetch_add(1) 的返回值 +1 即本次请求编号（SeqCst 全序）；
            // 写回前若已被更新的请求取代（编号不再相等）→ 陈旧结果丢弃（§7.5）。
            if PREVIEW_REQUEST_ID.load(Ordering::SeqCst) != id {
                return;
            }
            match result {
                Ok(img) => {
                    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Ready {
                        path: canonical_for_thread.clone(),
                        meta,
                        img: Arc::new(img),
                    };
                }
                Err(reason) => {
                    tracing::warn!(reason, "image preview: decode failed");
                    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Error {
                        path: canonical_for_thread,
                        reason: reason.to_string(),
                    };
                }
            }
            // 解码完成补帧（§0.3 心跳）：状态订阅者（overlay 组件）随之重渲染。
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        });
    if spawned.is_err() {
        tracing::warn!("image preview: decode thread spawn failed");
        *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Error {
            path: canonical,
            reason: "spawn".to_string(),
        };
    }
}

// ── meta 行文本（§7.4）───────────────────────────────────────────────────

/// 人类可读文件大小（与 T4 render.rs `human_size` 同一格式与 i18n keys——
/// B/KB/MB，1024 进制，KB/MB 一位小数）。
fn human_size(bytes: u64) -> String {
    let (key, value): (&str, FluentValue<'_>) = if bytes < 1024 {
        ("user-image-size-bytes", FluentValue::from(bytes))
    } else if bytes < 1024 * 1024 {
        (
            "user-image-size-kb",
            FluentValue::from(format!("{:.1}", bytes as f64 / 1024.0)),
        )
    } else {
        (
            "user-image-size-mb",
            FluentValue::from(format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))),
        )
    };
    i18n::tr_args(key, &[("count".to_string(), value)])
}

/// 文件名（§6.2-5 路径泄漏约束：meta 行不暴露绝对路径）。
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 状态 → meta 行文本（顶部边框内一行；进终端前已由调用方按宽度截断）。
/// - `Ready`：`[Image: 文件名 · WxH · 大小 · MIME]`（T5 `ImageMeta` 字段）；
/// - `Loading`：`[Image: 文件名]`（解码中，无尺寸）；
/// - `Degraded`：提示文案（手工路径降级）；
/// - `Error`：固定错误文案（不显示 reason——安全降级，§5.7）。
fn preview_meta_text(state: &ImagePreviewState) -> String {
    match state {
        ImagePreviewState::Idle => String::new(),
        ImagePreviewState::Loading { path, .. } => i18n::tr_args(
            "image-preview-loading",
            &[("name".to_string(), FluentValue::from(file_name_of(path)))],
        ),
        ImagePreviewState::Ready { path, meta, .. } => i18n::tr_args(
            "image-preview-meta",
            &[
                ("name".to_string(), FluentValue::from(file_name_of(path))),
                ("w".to_string(), FluentValue::from(u64::from(meta.width))),
                ("h".to_string(), FluentValue::from(u64::from(meta.height))),
                (
                    "size".to_string(),
                    FluentValue::from(human_size(meta.size_bytes)),
                ),
                ("mime".to_string(), FluentValue::from(meta.mime)),
            ],
        ),
        ImagePreviewState::Degraded { reason, .. } => reason.clone(),
        ImagePreviewState::Error { .. } => i18n::tr("image-preview-error"),
    }
}

// ── 绘制（post_component_draw；独立纯函数便于 TestBackend 单测）───────────

/// 绘制 overlay 一帧：边框（theme `component.popup` 样式，TUI-THEME-001）+
/// meta 行 + 像素区（`AsyncImage`，T6 窄接口）。
///
/// - `Idle` → 不绘制（占位符消失 → kitty 自动清理，S2 §7）；
/// - 终端小于 [`MIN_PREVIEW_TERM_W`]/[`MIN_PREVIEW_TERM_H`] 或像素区过小 →
///   仅边框 + meta 行（纯文本降级，§7.4）；
/// - `supported(caps)` 为 false（无协议/ITerm2 disabled）→ 保持文本降级
///   （§0.2 三层降级）。
fn draw_preview_overlay(
    buf: &mut Buffer,
    rect: Rect,
    state: &ImagePreviewState,
    pixel_enabled: bool,
    async_img: Option<&mut AsyncImage>,
    caps: &crate::kit::terminal_caps::TerminalCaps,
) {
    if matches!(state, ImagePreviewState::Idle) {
        return;
    }
    let popup = THEME_ATOM.state().read().component.popup;
    // [样式] 圆角边框弹窗（无背景填充，透明露出下层——用户要求不要背景色）。
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(popup.border));
    block.render(rect, buf);

    // meta 行：边框内顶部一行（display width 截断，TUI-TEXT-001）。
    // F3：统一过控制字符过滤（§6.2-4，与 T4 render.rs:908/934 同款）——
    // 文件名可含 ESC/换行（macOS 允许），不过滤则 escape 随 meta 行进入
    // 终端 buffer（终端控制序列注入）。sanitize 幂等，对固定 i18n 文案无副作用。
    let meta_text = preview_meta_text(state);
    let meta = sanitize_for_terminal(&meta_text);
    if !meta.is_empty() {
        let inner_w = usize::from(rect.width.saturating_sub(2).max(1));
        let text = truncate_by_width(&meta, inner_w);
        let sem = THEME_ATOM.state().read().semantic;
        buf.set_stringn(
            rect.x.saturating_add(1),
            rect.y.saturating_add(1),
            &text,
            inner_w,
            Style::default().fg(sem.text.primary),
        );
    }
    // 像素区：meta 行下方至下边框（§7.4）。
    if !pixel_enabled {
        return;
    }
    let pixel_area = Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(2),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(3),
    );
    if pixel_area.width < 2 || pixel_area.height < 1 {
        return;
    }
    if !matches!(state, ImagePreviewState::Ready { .. }) || !preview_supported(caps) {
        // [方案 B] 终端不支持图片协议（无协议/ITerm2 disabled）：像素区给出
        // 能力提示而非静默空白（grok 降级面板精神的精简版）；Degraded/Error
        // 状态不重复提示（meta 行已含原因文案，§5.7）。
        if matches!(state, ImagePreviewState::Ready { .. }) {
            paint_no_protocol_hint(buf, &pixel_area);
        }
        return;
    }
    if let Some(img) = async_img {
        img.render(pixel_area, buf);
    }
}

/// [方案 B] 终端不支持图片协议时，在像素区中央绘制一行能力提示
/// （i18n `image-preview-no-protocol`）。文本过 [`sanitize_for_terminal`]
/// （幂等，§6.2-4）+ 按像素区宽度截断（TUI-TEXT-001）。
fn paint_no_protocol_hint(buf: &mut Buffer, area: &Rect) {
    let raw = i18n::tr("image-preview-no-protocol");
    let hint = sanitize_for_terminal(&raw);
    if hint.is_empty() || area.width < 2 {
        return;
    }
    let inner_w = usize::from(area.width.saturating_sub(2).max(1));
    let text = truncate_by_width(&hint, inner_w);
    let x = area.x + (area.width.saturating_sub(text.width() as u16)) / 2;
    let y = area.y + area.height.saturating_div(2);
    let secondary = THEME_ATOM.state().read().semantic.text.secondary;
    buf.set_stringn(x, y, &text, inner_w, Style::default().fg(secondary));
}

// ── OverlayDrawHook：AsyncImage 生命周期 + 绘制 ──────────────────────────

/// overlay 绘制 Hook（跨帧持久）：
/// - `poll_change`：应用后台 resize 结果（§7.5）；`Ready` 时构造/切换
///   `AsyncImage`，非 `Ready` 时清理（drop → 后台线程自然退出，占位符从
///   buffer 消失即 kitty 自动清理，S2 §7）；状态转换返回 `Poll::Ready(())`
///   补帧；
/// - `post_component_draw`：在 Positioned（clear: true）及其子树绘制之后
///   渲染 overlay 内容（避免被 clear 擦除）。
struct OverlayDrawHook {
    /// 本帧几何（组件 body 每帧更新；None = 终端过小不渲染）。
    rect: Option<Rect>,
    /// 像素区门控（终端 ≥ 最小尺寸，§7.4）。
    pixel_enabled: bool,
    /// 当前解码结果对应的 `AsyncImage`（`Ready` 且路径匹配时存在）。
    async_img: Option<AsyncImage>,
    /// `async_img` 对应的路径（切换判定）。
    async_path: Option<PathBuf>,
}

impl OverlayDrawHook {
    fn new() -> Self {
        Self {
            rect: None,
            pixel_enabled: true,
            async_img: None,
            async_path: None,
        }
    }
}

impl Hook for OverlayDrawHook {
    fn poll_change(&mut self, _cx: &mut std::task::Context) -> Poll<()> {
        let mut redraw = false;
        // 后台 resize/encode 结果应用（§7.5 resize：UI 不阻塞）。
        if let Some(img) = self.async_img.as_mut() {
            redraw |= img.poll_completed();
        }
        let state = IMAGE_PREVIEW_STATE.state().read().clone();
        match &state {
            ImagePreviewState::Ready { path, img, .. } => {
                if self.async_path.as_ref() != Some(path) {
                    // 首帧/切换：重建协议（旧对象 drop → 后台线程自然退出）。
                    // F9：仅像素协议（Kitty）支持时才构造 Picker/AsyncImage——
                    // `picker_for` 的 halfblocks 构造在 TMUX 环境有 spawn 副作用，
                    // None/ITerm2 终端不得触达（image_preview.rs 注释）。
                    let caps = *crate::kit::atoms::TERMINAL_CAPS.state().read();
                    if preview_supported(&caps) {
                        let img = (**img).clone();
                        self.async_img = Some(AsyncImage::new(cached_picker(), img));
                        self.async_path = Some(path.clone());
                        redraw = true;
                    }
                }
            }
            _ => {
                // Idle/Loading/Degraded/Error：清理协议（隐藏/切换语义）。
                if self.async_img.take().is_some() {
                    self.async_path = None;
                    redraw = true;
                }
            }
        }
        if redraw {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let Some(rect) = self.rect else {
            return;
        };
        let state = IMAGE_PREVIEW_STATE.state().read().clone();
        let caps = *crate::kit::atoms::TERMINAL_CAPS.state().read();
        draw_preview_overlay(
            drawer.buffer_mut(),
            rect,
            &state,
            self.pixel_enabled,
            self.async_img.as_mut(),
            &caps,
        );
    }
}

// ── 组件 ──────────────────────────────────────────────────────────────────

/// F1 回归：是否用全尺寸 `clear: true` 的 Positioned 覆盖。
///
/// Idle（无预览，绝大多数时间）必须返回 `None`——`Positioned(clear: true)`
/// 每帧无条件把该区域清为背景，下层消息会被擦除（§7.5 隐藏应恢复原内容）。
/// 非 Idle 才有全尺寸 clear 覆盖（post_component_draw 在 clear 之后绘制
/// overlay 内容）。`None` → 组件渲染 0×0 `clear: false` 空覆盖
/// （同 [`crate::kit::popup_overlay::render_empty`] 模式，不参与布局）。
fn overlay_clear_rect(rect: Option<Rect>, state: &ImagePreviewState) -> Option<Rect> {
    match rect {
        Some(r) if !matches!(state, ImagePreviewState::Idle) => Some(r),
        _ => None,
    }
}

/// 上下文 overlay 预览组件（§7 T7）：订阅三态触发源，仲裁后经 effect 请求
/// 预览；几何与绘制在 [`OverlayDrawHook`]。渲染为空 `Positioned`（绝对坐标，
/// 不参与 flex）——实际内容由 hook 绘制。
#[component]
pub fn ImageOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅（稳定顺序，TUI-HOOK-001）：触发源 + 能力位 + 心跳 + 遮挡。
    let _hb = hooks.use_atom(&RENDER_HEARTBEAT);
    let _hover = hooks.use_atom(&IMAGE_HOVER);
    let _focus = hooks.use_atom(&FOCUSED_ENTRY);
    let _snapshot = hooks.use_atom(&INPUT_SNAPSHOT);
    let _vms = hooks.use_atom(&crate::kit::atoms::VIEW_MODELS);
    let preview = hooks.use_atom(&IMAGE_PREVIEW_STATE);
    let _caps = hooks.use_atom(&crate::kit::atoms::TERMINAL_CAPS);
    let _popup = hooks.use_atom(&crate::kit::atoms::POPUP_KIND);
    let _panel = hooks.use_atom(&crate::kit::atoms::ACTIVE_PANEL);
    let (term_w, term_h) = hooks.use_terminal_size();

    // 三态触发仲裁（§7.3；优先级 hover > cursor > focus；遮挡 → None）。
    let target = resolve_preview_target();
    // 请求流程（use_effect = effect 边界，非 render body——TUI-RENDER-001）。
    // 闭包捕获 target 副本；deps 用原值做变化比较（use_effect 只比较不消费）。
    let target_for_effect = target.clone();
    hooks.use_effect(
        move || request_preview(target_for_effect.as_deref()),
        target,
    );

    // 几何 + 像素门控（§7.4）。
    let rect = preview_geometry(term_w, term_h);
    let pixel_enabled = term_w >= MIN_PREVIEW_TERM_W && term_h >= MIN_PREVIEW_TERM_H;
    let draw = hooks.use_hook(OverlayDrawHook::new);
    draw.rect = rect;
    draw.pixel_enabled = pixel_enabled;

    // F1：Idle 恒走空覆盖（clear: false）——不清除终端中央区域下层内容。
    match overlay_clear_rect(rect, &preview.read()) {
        Some(r) => element! {
            Positioned(x: r.x, y: r.y, width: r.width, height: r.height, clear: true)
        },
        // 空覆盖——零尺寸 Positioned，避免参与父级布局。
        None => element!(Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false)),
    }
}

#[cfg(test)]
#[path = "image_overlay_test.rs"]
mod tests;
