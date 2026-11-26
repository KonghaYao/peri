//! composer 上方的待发送图片预览（真实终端图片协议）。
//!
//! 数据流：渲染 body 只做「取最后一张图片附件 → 查 `image_view` 缓存 → 写一帧
//! 预留几何」，base64 解码、尺寸校验与协议编码全部由 effect 启动的后台线程完成
//! （与消息区、工具卡共用同一缓存）；完成时 bump `RENDER_HEARTBEAT` 唤醒 InputArea
//! 重渲染，Ready 后预留高度收缩到图片实际行数。
//!
//! 门控：`protocol_available` 为 false（无协议 / ITerm2 / 缺单元格尺寸的 Sixel）
//! 时**不发起任何请求**，composer 保持 `[Image #n]` 文本标签。多图只预览最后一张，
//! 底部标签仍列出全部。

use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::{Constraint, Rect};

use crate::kit::atoms::{PENDING_ATTACHMENTS, PendingAttachment};
use crate::kit::image_preview::protocol_available;
use crate::kit::image_view::{self, Budget, ImageSource, UploadId};
use crate::kit::terminal_caps::TerminalCaps;

/// 预览区最大行数（Pending 时预留；Ready 后收缩到实际行数）。
pub(super) const PREVIEW_ROWS: u16 = 6;

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
///
/// 不再有 40 列上限：图片按输入区全宽等比铺满（最多 [`PREVIEW_ROWS`] 行），
/// 大图由行数上限约束内存与布局高度。
pub(super) fn preview_cols(composer_width: u16) -> u16 {
    let cols = composer_width.saturating_sub(2);
    if cols < MIN_PREVIEW_COLS { 0 } else { cols }
}

fn preview_budget(cols: u16) -> Budget {
    Budget::new(cols, PREVIEW_ROWS)
}

/// 预留高度：预算不足时整块让位（返回 0），保证 transcript 与 composer 可用。
///
/// `reserved` 是 composer + 弹出层已占用行数；预览不与 composer 争预算。
pub(super) fn layout_height(budget: Option<u16>, reserved: u16, rows: u16) -> u16 {
    if rows == 0 {
        return 0;
    }
    match budget {
        Some(budget) if reserved.saturating_add(rows) > budget => 0,
        _ => rows,
    }
}

/// 预留区矩形。
///
/// `area` 必须是**当帧**输入区矩形（`drawer.area`）。
/// composer 顶部 = 输入区顶 + 弹出层高度 + 队列高度，预览紧贴其上方；
/// 与 element! 中「队列与 composer 之间插入 View」的顺序一致。
/// 用上一帧几何会在输入区移动时错位，故不复用。
pub(super) fn preview_rect(area: Rect, y_offset: u16, height: u16) -> Option<Rect> {
    if height == 0 {
        return None;
    }
    let y = area.y.saturating_add(y_offset);
    if y.saturating_add(height) > area.bottom() {
        return None;
    }
    Some(Rect::new(area.x, y, area.width, height))
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

/// 一帧的预览结果（预留高度 + 附件身份）：占位高度与绘制都以它为准。
pub(super) struct PreviewPlan {
    pub(super) height: u16,
    source: Option<UploadId>,
}

/// InputArea 的预览入口：按缓存状态定高度并注册编码 effect。
///
/// 只在这里调用 `hooks.use_effect`，保证 hook 顺序与组件一致。
pub(super) fn use_preview(hooks: &mut Hooks<'_, '_>, inputs: PreviewInputs<'_>) -> PreviewPlan {
    let cols = preview_cols(inputs.composer_width);
    let source = target(inputs.attachments)
        .filter(|_| cols > 0)
        .map(PendingAttachment::upload_id);
    // 单元格像素尺寸只在真的有图片附件时才探测（`window_size` 是 ioctl）。
    let cell_px = source
        .as_ref()
        .and_then(|_| crate::kit::image_preview::cell_pixel_size());
    let protocol = source.is_some_and(|_| protocol_available(&inputs.caps, cell_px));
    let budget = preview_budget(cols);
    let (ready_rows, failed) = match (protocol, source) {
        (true, Some(id)) => (
            image_view::lookup(&ImageSource::Uploaded(id), budget).map(|placement| placement.rows),
            image_view::failed(&ImageSource::Uploaded(id), budget),
        ),
        _ => (None, false),
    };
    // Pending 预留满高，Ready 收缩到实际行数，失败不预留（避免永久空白条）。
    let rows = if !protocol || !inputs.visible {
        0
    } else if let Some(rows) = ready_rows {
        rows.min(PREVIEW_ROWS)
    } else if failed {
        0
    } else {
        PREVIEW_ROWS
    };
    let height = layout_height(inputs.budget, inputs.reserved, rows);
    let caps = inputs.caps;
    let request_source = source;
    hooks.use_effect(
        move || {
            if !protocol {
                return;
            }
            let atom = PENDING_ATTACHMENTS.state();
            let attachments = atom.read();
            if let Some(id) = target(&attachments).map(PendingAttachment::upload_id) {
                image_view::request(ImageSource::Uploaded(id), budget, &caps, cell_px);
            }
        },
        (request_source, cols, protocol, cell_px),
    );
    PreviewPlan { height, source }
}

/// 每帧写预留几何（钩子只读；`write_no_update` 避免渲染自激）。
pub(super) fn write_frame(frame: State<PreviewFrame>, plan: &PreviewPlan, y_offset: u16) {
    let mut frame = frame.write_no_update();
    frame.update(plan.height, y_offset, plan.source);
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
    source: Option<UploadId>,
}

impl PreviewFrame {
    pub(super) fn update(&mut self, height: u16, y_offset: u16, source: Option<UploadId>) {
        self.height = height;
        self.y_offset = y_offset;
        self.source = source;
    }

    /// 在输入区当帧区域 `area` 内绘制 Ready 的预览；其余状态 no-op。
    fn draw(&self, buf: &mut Buffer, area: Rect) {
        let Some(source) = self.source else {
            return;
        };
        let Some(rect) = preview_rect(area, self.y_offset, self.height) else {
            return;
        };
        let cols = preview_cols(area.width);
        let Some(placement) =
            image_view::lookup(&ImageSource::Uploaded(source), preview_budget(cols))
        else {
            return;
        };
        if let Some(target) = image_view::draw::centered_rect(rect, placement.cols, placement.rows)
        {
            image_view::draw::draw(buf, target, &placement.image);
        }
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
