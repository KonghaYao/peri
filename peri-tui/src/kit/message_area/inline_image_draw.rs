//! 消息区内联像素图片的绘制（`post_component_draw`）。
//!
//! 渲染 body 只把可见 slot 的图片放置折算成「槽内绝对视觉行」（纯几何，不触
//! IO）；[`InlineImageDrawHook`] 在 Paragraph 之后按当帧区域画像素协议。
//!
//! 滚动/重排每帧重算，图片随其预留行移动；只有**完整落在视口内**的图片才绘制
//! ——部分可见需要按行切片协议（`SlicedProtocol`）才能对齐，当前不做，避免把
//! 图片顶行画到视口边缘造成错位。图片滚出视口后占位符随 Paragraph 重绘为空白，
//! Kitty 的 unicode placeholder 消失即自动清理，不维护 id 表，也没有绝对定位残留。

use std::sync::Arc;

use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;

use super::selection::SlotIndex;
use super::vm_cache_slot::VmCacheSlot;
use crate::kit::image_preview::InlineImage;

/// 一帧内的图片放置（槽内绝对视觉行 + 消息区内的屏幕列）。
pub(super) struct InlineDraw {
    /// slot 内该图片首行的绝对视觉行（`slot_visual_start + visual_start`）。
    pub(super) vis_row: usize,
    /// 相对消息区 x=0 的正文列起点。
    pub(super) x: u16,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) image: Arc<InlineImage>,
}

/// 渲染 body 与绘制 hook 之间的单帧数据（`State` 句柄跨 hook 顺序共享）。
#[derive(Default)]
pub(super) struct InlineFrame {
    placements: Vec<InlineDraw>,
    scroll_y: usize,
    vis_height: u16,
    /// 当帧组件区域（`pre_component_draw` 写入，比渲染 body 读到的上一帧
    /// `area_rect` 新，resize 当帧也不会画到区域外）。
    area: Option<Rect>,
}

/// 收集可见 slot 的内联图片放置；返回项尚未换算成屏幕坐标。
pub(super) fn collect(
    caches: &[VmCacheSlot],
    index: &SlotIndex,
    scroll_y: usize,
    vis_height: u16,
) -> Vec<InlineDraw> {
    let mut out = Vec::new();
    if vis_height == 0 {
        return out;
    }
    let vp_end = scroll_y.saturating_add(usize::from(vis_height));
    for slot in index.visible_slots(scroll_y, vis_height as usize) {
        let Some(cache) = caches.get(slot) else {
            continue;
        };
        let slot_start = index.slot_visual_start(slot).unwrap_or(0);
        for info in &cache.image_lines {
            let Some(placement) = info.inline.as_ref() else {
                continue;
            };
            let Some(entry) = cache.wrap_map.get(info.logical_idx) else {
                continue;
            };
            let vis_row = slot_start.saturating_add(entry.visual_start);
            // 只保留完整落入视口的图片：部分可见时按行切片才能对齐（未接入）。
            if vis_row < scroll_y || vis_row.saturating_add(usize::from(placement.rows)) > vp_end {
                continue;
            }
            out.push(InlineDraw {
                vis_row,
                x: placement.x_offset,
                cols: placement.cols,
                rows: placement.rows,
                image: Arc::clone(&placement.image),
            });
        }
    }
    out
}

/// 渲染 body 每帧写入本帧放置；`area` 留待 `pre_component_draw` 更新。
pub(super) fn set_frame(
    frame: State<InlineFrame>,
    placements: Vec<InlineDraw>,
    scroll_y: usize,
    vis_height: u16,
) {
    let mut frame = frame.write_no_update();
    frame.placements = placements;
    frame.scroll_y = scroll_y;
    frame.vis_height = vis_height;
}

/// 清空放置（Welcome/空会话，防止 hook 继续画上一会话的图片）。
pub(super) fn clear(frame: State<InlineFrame>) {
    let mut frame = frame.write_no_update();
    frame.placements.clear();
    frame.vis_height = 0;
}

/// 绘制一帧内联图片；`clip` 是消息区当帧可见区域（已扣掉底部指示器）。
pub(super) fn draw_placements(buf: &mut Buffer, clip: Rect, scroll_y: usize, draws: &[InlineDraw]) {
    for draw in draws {
        if draw.cols == 0 || draw.rows == 0 {
            continue;
        }
        if let Some(rect) = crate::kit::image_view::draw::screen_rect(
            clip,
            draw.vis_row,
            scroll_y,
            draw.x,
            draw.cols,
            draw.rows,
        ) {
            crate::kit::image_view::draw::draw(buf, rect, &draw.image);
        }
    }
}

/// 跨帧持有一帧的放置与视口参数；绘制发生在 Paragraph 之后，避免被文字覆盖。
pub(super) struct InlineImageDrawHook {
    frame: State<InlineFrame>,
}

impl InlineImageDrawHook {
    pub(super) fn new(frame: State<InlineFrame>) -> Self {
        Self { frame }
    }
}

impl Hook for InlineImageDrawHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.frame.write_no_update().area = Some(drawer.area);
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let frame = self.frame.read();
        if frame.placements.is_empty() {
            return;
        }
        let Some(area) = frame.area else {
            return;
        };
        let clip = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.min(frame.vis_height),
        );
        draw_placements(drawer.buffer_mut(), clip, frame.scroll_y, &frame.placements);
    }
}
