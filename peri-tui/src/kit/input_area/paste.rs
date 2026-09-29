//! 输入区的粘贴处置：Ctrl+V 剪贴板与终端 bracketed paste。
//!
//! **图片走上传式**：剪贴板图片字节经 base64 直接进入 `PENDING_ATTACHMENTS`，
//! 提交/入队时以 `MessageContent::Blocks` 的 image block 上行。不再落盘
//! `~/.peri/images`，也不再插入 `@image <path>` 文本（程序生成面）。
//! `@image <path>` 文本形式仍可由用户手输——其读取面不在本模块。
//!
//! 阻塞系统 I/O（剪贴板读取、PNG 编码）全部在独立线程执行，不阻塞 UI 线程。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use fluent_bundle::FluentValue;
use ratatui_kit::prelude::{EventResult, State};

use crate::components::textarea::TextAreaState;
use crate::i18n;
use crate::kit::atoms::{
    NOTIFICATION, Notification, PENDING_ATTACHMENTS, PREDICTION, PendingAttachment, PredictionState,
};
use crate::kit::image_safety::MAX_IMAGE_BYTES;

use super::exit_entry_focus_on_edit;
use super::image::png_encode_bytes;
use super::popup::update_popup_prefix;
use super::submit::exit_history_mode_if_active;

/// 在启动线程前获取许可；释放覆盖正常返回、错误和 panic，避免重复按键叠加整图分配。
#[derive(Default, Clone)]
pub(super) struct PasteGate(Arc<AtomicBool>);

pub(super) struct PastePermit(Arc<AtomicBool>);

impl PasteGate {
    pub(super) fn try_acquire(&self) -> Option<PastePermit> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| PastePermit(self.0.clone()))
    }
}

impl Drop for PastePermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn notify(message: String, secs: u64) {
    *NOTIFICATION.state().write() = Some(Notification {
        message,
        until: std::time::Instant::now() + std::time::Duration::from_secs(secs),
    });
}

/// 标准 base64（与 `ContentBlock::Image` 的 wire 载荷一致）。
fn base64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// 图片字节 → 待发送附件（上传式：base64 直接进 composer，不落盘）。
fn attach_image(bytes: Vec<u8>, media_type: &str) {
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image(media_type, base64_encode(&bytes)));
}

/// Ctrl+V：剪贴板 → 输入区。
///
/// 顺序：macOS 原生 PNG → arboard RGBA（编码为 PNG）→ 文本。图片成功即进
/// `PENDING_ATTACHMENTS`，不修改编辑器文本。粘贴不触发 slash/mention 弹窗。
pub(super) fn handle_clipboard_paste(state: State<TextAreaState>, gate: &PasteGate) -> EventResult {
    let Some(permit) = gate.try_acquire() else {
        notify(i18n::tr("paste-in-progress"), 2);
        return EventResult::Consumed;
    };
    exit_history_mode_if_active();
    exit_entry_focus_on_edit();
    std::thread::spawn(move || {
        let _permit = permit;
        #[cfg(target_os = "macos")]
        match read_pasteboard_png_bytes() {
            Ok(Some(bytes)) => {
                attach_image(bytes, "image/png");
                return;
            }
            Ok(None) => {}
            Err(_) => {
                notify(i18n::tr("paste-image-failed"), 4);
                return;
            }
        }
        // ── 图片粘贴分支（arboard RGBA → PNG）──
        // arboard 的 get_image() 需要新的 Clipboard 实例（之前的 cb 可能已被消费）
        if let Some(arboard::ImageData {
            bytes: image_bytes,
            width,
            height,
        }) = arboard::Clipboard::new()
            .ok()
            .and_then(|mut clipboard| clipboard.get_image().ok())
        {
            // arboard may already own the clipboard allocation.  Move it out
            // instead of cloning every RGBA byte before encoding.
            let rgba = image_bytes.into_owned();
            if rgba.is_empty() {
                return;
            }
            match png_encode_bytes(&rgba, width, height, max_upload_bytes()) {
                Ok(png) => {
                    attach_image(png, "image/png");
                    return;
                }
                Err(_) => {
                    // 编码失败或超过上传上限：明确告知（不再静默丢弃图片）。
                    notify(i18n::tr("paste-image-failed"), 4);
                    return;
                }
            }
        }

        // ── 文本粘贴分支 ──
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return;
        };
        let Ok(text) = clipboard.get_text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        insert_pasted_text(&state, &text);
    });
    *PREDICTION.state().write() = PredictionState::default();
    EventResult::Consumed
}

/// 终端 bracketed paste（`Event::Paste`）：归一化换行 + 上限截断后插入。
pub(super) fn handle_bracketed_paste(state: State<TextAreaState>, paste_text: &str) -> EventResult {
    // I22-A：paste 大小上限——防止用户误粘 10MB 日志冻结终端。
    // 10_000 chars 足够覆盖正常长 paste（代码片段、命令输出）；
    // 超出截断并 log warn 提示（用户可改用文件追加方式）。
    const MAX_PASTE_CHARS: usize = 10_000;
    // 部分终端（VSCode、iTerm2）在 Bracketed Paste 中使用 \r 作为
    // 换行分隔符；render_multiline_with_cursor 只按 \n 拆分行，
    // 未归一化的 \r 会导致换行在渲染时不可见。
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let char_count = normalized.chars().count();
    if char_count > MAX_PASTE_CHARS {
        tracing::warn!(
            original_chars = char_count,
            capped_at = MAX_PASTE_CHARS,
            "InputArea: paste 截断——超出 10K char 上限"
        );
    }
    let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
    exit_entry_focus_on_edit();
    let mut s = state.write();
    s.insert_str(&truncated);
    update_popup_prefix(&s);
    *PREDICTION.state().write() = PredictionState::default();
    EventResult::Consumed
}

/// 插入粘贴文本：超过 10K 字符截断并通知（Ctrl+V 文本分支语义）。
fn insert_pasted_text(state: &State<TextAreaState>, text: &str) {
    const MAX: usize = 10_000;
    let total = text.chars().count();
    if total > MAX {
        notify(
            i18n::tr_args(
                "paste-truncated",
                &[("max".into(), FluentValue::from(MAX as i64))],
            ),
            2,
        );
        let truncated: String = text.chars().take(MAX).collect();
        state.write().insert_str(&truncated);
    } else {
        state.write().insert_str(text);
    }
}

/// 上传字节上限（与 ImageMiddleware 的 20MB 校验同源）。
fn max_upload_bytes() -> usize {
    MAX_IMAGE_BYTES as usize
}

/// macOS 原生剪贴板 PNG 字节。
///
/// PNG 缺席返回 `Ok(None)`（调用方回落到 arboard 的 RGBA 路径）；PNG 存在但
/// 超限或头部非法则返回 `Err`——超限/坏数据不得触发更昂贵的解码路径。
#[cfg(target_os = "macos")]
pub(super) fn read_pasteboard_png_bytes() -> anyhow::Result<Option<Vec<u8>>> {
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        pasteboard_png_bytes(&pasteboard)
    })
}

#[cfg(target_os = "macos")]
fn pasteboard_png_bytes(
    pasteboard: &objc2_app_kit::NSPasteboard,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(data) = (unsafe { pasteboard.dataForType(objc2_app_kit::NSPasteboardTypePNG) }) else {
        return Ok(None);
    };
    anyhow::ensure!(
        data.len() as u64 <= MAX_IMAGE_BYTES,
        "clipboard PNG exceeds byte limit"
    );
    // SAFETY: 保持 NSData 存活且不修改内容，借用仅在同步校验与拷贝期间有效。
    let bytes = unsafe { data.as_bytes_unchecked() };
    // 只读 IHDR，不分配像素缓冲；完整解码仍由使用图片的受限入口负责。
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.read_header_info()?;
    Ok(Some(bytes.to_vec()))
}

#[cfg(test)]
#[path = "paste_test.rs"]
mod tests;
