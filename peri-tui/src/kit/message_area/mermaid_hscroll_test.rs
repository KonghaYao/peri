//! 验收：宽 Mermaid 帧在消息区真实 Buffer 中的横向条、命中与拖拽换算。

use std::sync::Arc;

use super::grid::GridSpec;
use super::hits::build_mermaid_hscroll_hits;
use super::mermaid_scroll::{mermaid_arrow_delta, mermaid_wheel_delta, scroll_step, set_position};
use super::transcript::Transcript;
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use peri_theme::atoms::THEME_ATOM;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;

const WIDE: &str = "sequenceDiagram\n  participant Client\n  participant Server\n  participant Loom\n  participant Rivet\n  Client->>Server: POST /v1/events with a fairly long request name\n  Server->>Loom: send(action), key=agent/<logicalId>\n  Loom->>Rivet: locate actor and forward";
const NARROW: &str = "graph TD\nA-->B";

fn context() -> crate::kit::entry_render_cache::EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    crate::kit::entry_render_cache::EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn bubble(source: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: format!("```mermaid\n{source}\n```"),
            reasoning: None,
            message_id: Some("mermaid-hscroll".to_string()),
            content_hash: 3_001 + source.len() as u64,
        }
        .into(),
    )
}

struct Harness {
    snapshot: ViewModelsSnapshot,
    transcript: Transcript,
    caches: Vec<VmCacheSlot>,
    grid: GridSpec,
    context: crate::kit::entry_render_cache::EntryRenderContext,
}

impl Harness {
    fn new(source: &str, width: u16) -> Self {
        Self {
            snapshot: ViewModelsSnapshot {
                generation: 1,
                items: vec![bubble(source)].into(),
            },
            transcript: Transcript::default(),
            caches: Vec::new(),
            grid: GridSpec::grid_for(width),
            context: context(),
        }
    }

    fn layout(&mut self) -> Arc<super::selection::SlotIndex> {
        let publication = crate::kit::atoms::TranscriptPublication::default();
        self.transcript.prepare(
            &self.snapshot,
            &publication,
            &mut self.caches,
            self.grid,
            self.grid.line_width(),
            self.context.clone(),
            0,
            0,
            0,
            40,
            0,
        );
        self.transcript
            .warm_visible(&self.snapshot, &mut self.caches, 0, 40);
        crate::kit::mermaid::drain_preview_jobs_now();
        let index = self.transcript.prepare(
            &self.snapshot,
            &publication,
            &mut self.caches,
            self.grid,
            self.grid.line_width(),
            self.context.clone(),
            0,
            0,
            0,
            40,
            0,
        );
        self.transcript
            .warm_visible(&self.snapshot, &mut self.caches, 0, 40);
        index
    }
}

fn row_text(buffer: &Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|column| buffer[(column, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn render_slot(slot: &VmCacheSlot, width: u16) -> Buffer {
    let lines = slot.lines.as_ref().expect("materialized slot lines");
    let rows: Vec<_> = (0..slot.wrap_map.len())
        .filter_map(|index| lines.line(index).cloned())
        .collect();
    let height = u16::try_from(rows.len()).expect("test message fits u16 rows");
    let mut terminal =
        ratatui::Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new(Text::from(rows)), frame.area());
        })
        .expect("render slot");
    terminal.backend().buffer().clone()
}

#[test]
#[serial_test::serial]
fn mounted_message_area_paints_the_bar_in_a_real_buffer() {
    crate::kit::mermaid::reset_for_test();
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    let previous_session = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let previous_vms = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "session".to_string();
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot {
        generation: 1,
        items: vec![bubble(WIDE)].into(),
    };

    let grid = GridSpec::grid_for(80);
    let _ = ratatui_kit::test_util::render_frames(
        ratatui_kit::element!(super::MessageArea(grid: grid)),
        80,
        40,
        2,
    );
    // 首次渲染登记/排队预览；无后台 worker 时同步重建，有 worker 时排空后再渲染。
    crate::kit::mermaid::drain_preview_jobs_now();
    let buffer = ratatui_kit::test_util::render_frames(
        ratatui_kit::element!(super::MessageArea(grid: grid)),
        80,
        40,
        2,
    );

    let rows: Vec<String> = (0..buffer.area.height)
        .map(|row| row_text(&buffer, row))
        .collect();
    assert!(
        rows.iter()
            .any(|row| row.contains('◄') && row.contains('►')),
        "消息区真实 Buffer 应包含横向滚动条: {rows:#?}"
    );

    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = previous_session;
    *crate::kit::atoms::VIEW_MODELS.state().write() = previous_vms;
}

#[test]
#[serial_test::serial]
fn painted_bar_matches_hit_geometry_and_drags_to_both_ends() {
    crate::kit::mermaid::reset_for_test();
    let mut harness = Harness::new(WIDE, 80);
    let index = harness.layout();
    let slot = &harness.caches[0];
    let span = slot
        .mermaid_fences
        .first()
        .expect("assistant bubble has one fence");
    let view = span
        .h_view
        .expect("wide sequence overflows 74 content columns");
    assert!(view.max_offset > 0);

    let width = harness.grid.line_width();
    let buffer = render_slot(slot, width);
    let area = Rect::new(0, 0, width, buffer.area.height);
    let hits = build_mermaid_hscroll_hits(&harness.caches, &index, Some(area), area.height, 0);
    let hit = hits
        .iter()
        .find(|hit| hit.key == span.key)
        .expect("visible wide frame registers a hit");
    let bar = hit.bar.expect("bar row exists at 74 columns");
    let bar_row = hit.bar_row.expect("bar row inside viewport");
    let painted_row = row_text(&buffer, bar_row);
    assert!(
        painted_row.contains('◄') && painted_row.contains('►'),
        "真实 Buffer 底部应有横向条: {painted_row:?}"
    );

    // 绘制与命中同源：每个轨道格的 thumb 背景必须与 HorizontalBar 一致。
    let dim = THEME_ATOM.state().read().semantic.text.dim;
    for cell in 0..bar.track {
        let column = hit.x_start + 1 + cell as u16;
        let painted = buffer[(column, bar_row)].bg == dim;
        let expected = (bar.thumb_start..bar.thumb_start + bar.thumb_len).contains(&cell);
        assert_eq!(painted, expected, "track cell {cell}");
    }

    // 按在轨道右端：thumb 以指针为中心跳到右端；拖回左端回到 0。
    let content = view.viewport + view.max_offset;
    let right_cell = bar.track - 2;
    let grab = bar.grab(right_cell).unwrap_or(bar.thumb_len / 2);
    let press_offset = if bar.grab(right_cell).is_some() {
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, content)
    } else {
        bar.offset_for(right_cell.saturating_sub(grab))
    };
    assert!(crate::kit::mermaid::set_h(
        &hit.key,
        press_offset,
        hit.viewport,
        content
    ));
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, content),
        hit.max_offset,
        "按在右端应到达最右列"
    );
    assert!(crate::kit::mermaid::set_h(
        &hit.key,
        bar.offset_for(0),
        hit.viewport,
        content
    ));
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, content),
        0,
        "拖回左端应回到 0"
    );
}

#[test]
#[serial_test::serial]
fn wheel_and_arrow_steps_share_the_visible_hit_region() {
    crate::kit::mermaid::reset_for_test();
    let mut harness = Harness::new(WIDE, 80);
    let index = harness.layout();
    let slot = &harness.caches[0];
    let span = &slot.mermaid_fences[0];
    let view = span.h_view.expect("wide frame");
    let content = view.viewport + view.max_offset;
    let width = harness.grid.line_width();
    let buffer = render_slot(slot, width);
    let area = Rect::new(0, 0, width, buffer.area.height);
    let hits = build_mermaid_hscroll_hits(&harness.caches, &index, Some(area), area.height, 0);
    let hit = &hits[0];

    // 指针在帧内任意正文行都可平移；在帧外不响应。
    assert!(hit.contains(hit.x_start, hit.row_start));
    assert!(!hit.contains(hit.x_start.saturating_sub(1), hit.row_start));
    assert!(!hit.contains(hit.x_start, hit.row_end));
    assert_eq!(
        mermaid_wheel_delta(MouseEventKind::ScrollDown, true),
        Some(8),
        "Shift+滚轮横向平移"
    );
    assert_eq!(mermaid_wheel_delta(MouseEventKind::ScrollDown, false), None);
    assert_eq!(
        mermaid_wheel_delta(MouseEventKind::ScrollRight, false),
        Some(8)
    );
    assert_eq!(mermaid_arrow_delta(KeyCode::Left), Some(-8));
    assert_eq!(mermaid_arrow_delta(KeyCode::Right), Some(8));

    // 连续 Shift+滚轮在右端 clamp。
    for _ in 0..40 {
        crate::kit::mermaid::scroll_h(
            &hit.key,
            mermaid_wheel_delta(MouseEventKind::ScrollDown, true).expect("shift wheel"),
            hit.viewport,
            content,
        );
    }
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, content),
        hit.max_offset
    );
    for _ in 0..40 {
        crate::kit::mermaid::scroll_h(
            &hit.key,
            mermaid_arrow_delta(KeyCode::Left).expect("left arrow"),
            hit.viewport,
            content,
        );
    }
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, content),
        0
    );
}

#[test]
#[serial_test::serial]
fn state_survives_history_replay_and_normal_width_has_no_hit() {
    crate::kit::mermaid::reset_for_test();
    let mut harness = Harness::new(WIDE, 80);
    let index = harness.layout();
    let view = harness.caches[0].mermaid_fences[0]
        .h_view
        .expect("wide frame");
    let key = harness.caches[0].mermaid_fences[0].key.clone();
    assert!(crate::kit::mermaid::set_h(
        &key,
        24,
        view.viewport,
        view.viewport + view.max_offset
    ));
    let area = Rect::new(0, 0, harness.grid.line_width(), 40);
    let first = build_mermaid_hscroll_hits(&harness.caches, &index, Some(area), 40, 0);
    assert_eq!(first.len(), 1);

    // 新 Transcript（历史重放）读取同一身份：偏移仍在。
    let mut replay = Harness::new(WIDE, 80);
    let _ = replay.layout();
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&key, view.viewport, view.viewport + view.max_offset),
        24
    );

    // 正常宽度图不注册横向命中，也不出现滚动条。
    let mut narrow = Harness::new(NARROW, 80);
    let narrow_index = narrow.layout();
    let narrow_slot = &narrow.caches[0];
    assert!(narrow_slot.mermaid_fences[0].h_view.is_none());
    let buffer = render_slot(narrow_slot, narrow.grid.line_width());
    let area = Rect::new(0, 0, narrow.grid.line_width(), buffer.area.height);
    assert!(
        build_mermaid_hscroll_hits(&narrow.caches, &narrow_index, Some(area), area.height, 0)
            .is_empty()
    );
    let text: String = (0..buffer.area.height)
        .map(|row| row_text(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains('◄') && !text.contains('►'), "{text}");
}

/// 回归：handler 平移必须按内容宽（viewport + max_offset）调用 set_h/scroll_h。
/// 直接把最大偏移当内容宽时 viewport 更大，偏移会被恒 clamp 到 0。
#[test]
#[serial_test::serial]
fn handler_pan_reaches_the_right_edge_for_wide_frames() {
    crate::kit::mermaid::reset_for_test();
    let mut harness = Harness::new(WIDE, 80);
    let index = harness.layout();
    let area = Rect::new(0, 0, harness.grid.line_width(), 40);
    let hits = build_mermaid_hscroll_hits(&harness.caches, &index, Some(area), 40, 0);
    let hit = hits.first().expect("wide frame registers a hit");
    assert!(hit.max_offset > 0, "帧必须溢出才有横向平移");

    assert!(set_position(
        &hit.key,
        usize::MAX,
        hit.viewport,
        hit.max_offset
    ));
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, hit.viewport + hit.max_offset),
        hit.max_offset,
        "定位必须能到达最右列"
    );

    assert!(set_position(&hit.key, 0, hit.viewport, hit.max_offset));
    assert!(scroll_step(
        &hit.key,
        i32::MAX,
        hit.viewport,
        hit.max_offset
    ));
    assert_eq!(
        crate::kit::mermaid::h_offset_for(&hit.key, hit.viewport, hit.viewport + hit.max_offset),
        hit.max_offset,
        "键盘/滚轮步进也必须能到达最右列"
    );
}
