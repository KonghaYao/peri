//! 消息区内联像素图片的几何与绘制回归。
//!
//! 覆盖：占位行与真实编码行数一致、Rect 随滚动移动、完整可见才绘制、
//! 滚出视口后当帧不再写入任何转义（占位符消失 → Kitty 自动清理）、
//! 区域外（resize/位移）不越界绘制。

use std::sync::Arc;

use super::grid::GridSpec;
use super::inline_image_draw::{collect, draw_placements};
use super::selection::SlotIndex;
use super::transcript::Transcript;
use super::vm_cache_slot::VmCacheSlot;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use peri_theme::atoms::THEME_ATOM;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn context() -> EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn bubble(text: String) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text,
            reasoning: None,
            message_id: Some("inline-image-draw".to_string()),
            content_hash: 7,
        }
        .into(),
    )
}

struct Harness {
    snapshot: ViewModelsSnapshot,
    transcript: Transcript,
    caches: Vec<VmCacheSlot>,
    grid: GridSpec,
    context: EntryRenderContext,
}

impl Harness {
    fn new(vm: TuiRenderUnit, width: u16) -> Self {
        let grid = GridSpec::grid_for(width);
        Self {
            snapshot: ViewModelsSnapshot {
                generation: 1,
                items: vec![vm].into(),
            },
            transcript: Transcript::default(),
            caches: Vec::new(),
            grid,
            context: context(),
        }
    }

    fn layout(&mut self, scroll_y: usize, height: usize) -> Arc<SlotIndex> {
        self.transcript.prepare(
            &self.snapshot,
            &crate::kit::atoms::TranscriptPublication::default(),
            &mut self.caches,
            self.grid,
            self.grid.line_width(),
            self.context.clone(),
            0,
            0,
            scroll_y,
            height,
            0,
        );
        self.transcript
            .warm_visible(&self.snapshot, &mut self.caches, scroll_y, height)
    }
}

fn has_escape(buf: &Buffer) -> bool {
    let area = buf.area;
    (area.top()..area.bottom())
        .any(|y| (area.left()..area.right()).any(|x| buf[(x, y)].symbol().contains('\u{1b}')))
}

fn has_halfblock(buf: &Buffer) -> bool {
    let area = buf.area;
    (area.top()..area.bottom()).any(|y| {
        (area.left()..area.right()).any(|x| {
            buf[(x, y)]
                .symbol()
                .chars()
                .any(|c| matches!(c, '\u{2580}' | '\u{2584}' | '\u{2588}'))
        })
    })
}

/// 写入一张本地 PNG 并返回 markdown 可用的来源文本。
fn red_png(dir: &tempfile::TempDir) -> String {
    let file = dir.path().join("red.png");
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([220, 30, 30, 255]));
    image
        .save_with_format(&file, image::ImageFormat::Png)
        .expect("inline image draw: png fixture must encode");
    file.to_str().expect("utf8 path").replace('\\', "/")
}

fn message_with_image(source: &str) -> TuiRenderUnit {
    // 图片在最前、后续填充行用于滚动；正文超过 120 字符，避免走 brief 收窄宽度。
    bubble(format!(
        "![a]({source})\n\n{}",
        "filler line for scrolling\n\n".repeat(12)
    ))
}

#[test]
fn placement_covers_encoded_rows_and_moves_with_scroll() {
    let dir = tempfile::tempdir().expect("inline image draw: tempdir must exist");
    let source = red_png(&dir);
    let width = 80u16;
    let height = 14u16;
    let grid = GridSpec::grid_for(width);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(
        crate::kit::image_view::request_until_ready(
            crate::kit::image_view::ImageSource::File(&source),
            crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::MAX_ROWS),
            std::time::Duration::from_secs(5)
        ),
        "图片缓存应在布局前就绪"
    );

    let mut harness = Harness::new(message_with_image(&source), width);
    let index = harness.layout(0, usize::from(height));
    let inline = {
        let slot = &harness.caches[0];
        let info = slot
            .image_lines
            .iter()
            .find(|info| info.inline.is_some())
            .expect("图片段应产出内联放置");
        let placement = info.inline.as_ref().expect("inline");
        let entry = slot.wrap_map.get(info.logical_idx).expect("wrap entry");
        (info.logical_idx, entry.visual_start, placement.rows)
    };
    assert!(inline.2 >= 1, "图片至少占 1 行");

    let placements = {
        let slot_start = index.slot_visual_start(0).unwrap_or(0);
        let out = collect(&harness.caches, &index, 0, height);
        assert_eq!(out.len(), 1, "完整可见时应产生一处放置");
        assert_eq!(
            out[0].vis_row,
            slot_start + inline.1,
            "放置行 = 占位首行的绝对视觉行"
        );
        assert_eq!(usize::from(out[0].rows), usize::from(inline.2));
        out
    };

    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    draw_placements(&mut buf, area, 0, &placements);
    assert!(has_escape(&buf), "应写入终端图片协议转义");
    assert!(!has_halfblock(&buf), "不得退化为半块字符");
    let x = placements[0].x;
    let y = u16::try_from(placements[0].vis_row).expect("row fits u16");
    assert!(
        buf[(x, y)].symbol().contains('_'),
        "图片应从正文列开始写入协议串"
    );

    // 滚动使图片完全离开视口：新帧不再产生放置，缓冲区里也没有残留转义。
    let scrolled = inline.1 + usize::from(inline.2);
    let index = harness.layout(scrolled, usize::from(height));
    let out = collect(&harness.caches, &index, scrolled, height);
    assert!(out.is_empty(), "滚出视口后不应再绘制图片");
    let mut next = Buffer::empty(area);
    draw_placements(&mut next, area, scrolled, &out);
    assert!(!has_escape(&next), "滚出后当帧不得残留图片转义");
}

#[test]
fn partially_visible_image_is_not_drawn() {
    let dir = tempfile::tempdir().expect("inline image draw: tempdir must exist");
    let source = red_png(&dir);
    let width = 80u16;
    let height = 12u16;
    let grid = GridSpec::grid_for(width);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(crate::kit::image_view::request_until_ready(
        crate::kit::image_view::ImageSource::File(&source),
        crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::MAX_ROWS),
        std::time::Duration::from_secs(5)
    ));

    let mut harness = Harness::new(message_with_image(&source), width);
    let _ = harness.layout(0, usize::from(height));
    // 偏移 1 行：首行已滚出视口，剩下部分没有对齐手段——按设计跳过整图。
    let index = harness.layout(1, usize::from(height));
    let out = collect(&harness.caches, &index, 1, height);
    assert!(out.is_empty(), "部分可见的图片当前不绘制（避免错位）");
}

#[test]
fn draw_is_clipped_to_shrunken_area() {
    let dir = tempfile::tempdir().expect("inline image draw: tempdir must exist");
    let source = red_png(&dir);
    let width = 80u16;
    let height = 12u16;
    let grid = GridSpec::grid_for(width);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(crate::kit::image_view::request_until_ready(
        crate::kit::image_view::ImageSource::File(&source),
        crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::MAX_ROWS),
        std::time::Duration::from_secs(5)
    ));

    let mut harness = Harness::new(message_with_image(&source), width);
    let index = harness.layout(0, usize::from(height));
    let placements = collect(&harness.caches, &index, 0, height);
    assert_eq!(placements.len(), 1);

    // 区域高度缩到「图片首行 + 1 行」（resize 当帧）：图片底部越界 → 不绘制，
    // 下一帧按新高度重新布局。避免把图片画到消息区之外。
    let vis_row = u16::try_from(placements[0].vis_row).expect("row fits u16");
    let short = Rect::new(0, 0, width, vis_row.saturating_add(1));
    let mut buf = Buffer::empty(short);
    draw_placements(&mut buf, short, 0, &placements);
    assert!(!has_escape(&buf), "越界图片不得绘制到区域外");
}

/// 组件级回归：挂载 MessageArea 后，最终 Buffer 里出现终端协议转义而不是
/// 半块字符——确认 hook 在 Paragraph 之后绘制，且没有被文字覆盖。
#[test]
#[serial_test::serial]
fn mounted_message_area_paints_kitty_escape() {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().expect("inline image draw: tempdir must exist");
    let source = red_png(&dir);
    let grid = GridSpec::grid_for(80);
    let cols = u16::try_from(grid.content_width()).expect("content width fits u16");
    assert!(crate::kit::image_view::request_until_ready(
        crate::kit::image_view::ImageSource::File(&source),
        crate::kit::image_view::Budget::new(cols, crate::kit::inline_image::MAX_ROWS),
        std::time::Duration::from_secs(5)
    ));

    let previous_session = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let previous_vms = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "inline-image".to_string();
    // 消息整体短于视口（自动跟随底部时图片首行仍在屏内）；正文 >120 字符避开 brief。
    let short = bubble(format!("![a]({source})\n\n{}", "x".repeat(140)));
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot {
        generation: 1,
        items: vec![short].into(),
    };

    let buffer = ratatui_kit::test_util::render_frames(
        ratatui_kit::element!(super::MessageArea(grid: grid)),
        80,
        30,
        2,
    );
    assert!(
        has_escape(&buffer),
        "挂载后的消息区 Buffer 应包含 Kitty 协议转义"
    );
    assert!(!has_halfblock(&buffer), "消息区图片不得退化为半块字符");

    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = previous_session;
    *crate::kit::atoms::VIEW_MODELS.state().write() = previous_vms;
}
