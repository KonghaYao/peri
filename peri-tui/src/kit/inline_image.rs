//! 消息区图片的视图模型适配：扫描图片语法 / 上传附件 / 工具图片并发起请求。
//!
//! 图片组件（几何、缓存、编码、上传仓库）在 [`crate::kit::image_view`]；这里只做
//! 「这条消息有哪些图、各自预算多少」的扫描，供消息区 effect 在事件边界调用。
//! 渲染 body 只查 `image_view::lookup`，不经过本模块。

use crate::kit::atoms::TERMINAL_CAPS;
use crate::kit::image_preview;
use crate::kit::image_view::{self, Budget, ImageSource, UploadId};
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_tui_components::markdown::image_links;

/// 转录区正文图片（assistant markdown 独占行 / 用户手输 `![...]`）的最大行数。
pub(crate) const MAX_ROWS: u16 = 8;

/// 用户气泡单张图片的行预算（上传附件与手输图片共用）。
pub(crate) const USER_IMAGE_ROWS: u16 = 6;

/// 用户气泡全部图片的总行预算：超出后附件/图片只保留 meta 行。
pub(crate) const USER_IMAGE_TOTAL_ROWS: u16 = 10;

/// 工具卡单张图片的行预算。
pub(crate) const TOOL_IMAGE_ROWS: u16 = 8;

/// 工具卡全部图片的总行预算：超出后剩余图片降级为 meta 行。
pub(crate) const TOOL_IMAGE_TOTAL_ROWS: u16 = 16;

/// 单个工具卡最多渲染/请求的图片数（一次工具调用返回大量图片时限制编码线程）。
pub(crate) const TOOL_IMAGE_MAX: usize = 4;

/// 扫描一条消息中的图片并发起请求（effect 边界调用，非 render body）。
///
/// - assistant 走完整 markdown 扫描（整行仅图片语法与空白）；
/// - 用户气泡：手输图片行 + 上传附件（摘要查询，载荷在 `image_view` 仓库）；
/// - 工具卡：事件边界登记过的工具图片。
pub(crate) fn request_vm_images(vm: &TuiRenderUnit, content_cols: u16, process_cols: u16) {
    let caps = *TERMINAL_CAPS.state().read();
    let cell_px = image_preview::cell_pixel_size();
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            let cols = if bubble.is_brief() {
                process_cols
            } else {
                content_cols
            };
            // 只请求渲染器可能替换的独占行图片（整行仅图片语法与空白）。
            for line in bubble.text.lines() {
                for url in image_line_urls(line) {
                    image_view::request(
                        ImageSource::File(&url),
                        Budget::new(cols, MAX_ROWS),
                        &caps,
                        cell_px,
                    );
                }
            }
        }
        TuiRenderUnit::TuiUserBubble(bubble) => {
            request_user_images(bubble, content_cols, &caps, cell_px);
        }
        TuiRenderUnit::TuiToolCard(card) => {
            for id in card.images.iter().take(TOOL_IMAGE_MAX) {
                request_tool_image(*id, process_cols, &caps, cell_px);
            }
        }
        _ => {}
    }
}

fn request_user_images(
    bubble: &crate::kit::tui_render_unit::TuiUserBubble,
    content_cols: u16,
    caps: &crate::kit::terminal_caps::TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    let budget = Budget::new(content_cols, USER_IMAGE_ROWS);
    for line in bubble.text.lines() {
        if let Some(url) = standalone_image_url(line) {
            image_view::request(ImageSource::File(&url), budget, caps, cell_px);
        }
    }
    for attachment in &bubble.attachments {
        image_view::request(
            ImageSource::Uploaded(attachment.upload_id()),
            budget,
            caps,
            cell_px,
        );
    }
}

/// 工具图片请求（事件边界或 `request_vm_images` 共用）。
pub(crate) fn request_tool_image(
    id: UploadId,
    cols: u16,
    caps: &crate::kit::terminal_caps::TerminalCaps,
    cell_px: Option<(u16, u16)>,
) {
    image_view::request(
        ImageSource::Uploaded(id),
        Budget::new(cols, TOOL_IMAGE_ROWS),
        caps,
        cell_px,
    );
}

/// 该 VM 是否可能含图片；仅用于缓存失效门控（可能含代码块内的假阳性）。
#[must_use]
pub(crate) fn vm_may_have_image(vm: &TuiRenderUnit) -> bool {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.text.contains("!["),
        TuiRenderUnit::TuiUserBubble(bubble) => {
            bubble.text.contains("![") || !bubble.attachments.is_empty()
        }
        TuiRenderUnit::TuiToolCard(card) => !card.images.is_empty(),
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

#[cfg(test)]
#[path = "inline_image_test.rs"]
mod tests;
