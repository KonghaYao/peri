//! 第 3 部分验收：真实视口相交调度、几何复用与阅读锚点。

use std::sync::Arc;

use super::grid::GridSpec;
use super::selection::SlotIndex;
use super::transcript::Transcript;
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use peri_theme::atoms::THEME_ATOM;

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

fn bubble(index: usize, source: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: format!(
                "```mermaid\n{source}\n```\n\n{}",
                "filler line\n\n".repeat(10)
            ),
            reasoning: None,
            message_id: Some(format!("mermaid-{index}")),
            content_hash: (index as u64 + 1) * 1000 + source.len() as u64,
        }
        .into(),
    )
}

fn snapshot(sources: &[String]) -> ViewModelsSnapshot {
    ViewModelsSnapshot {
        generation: 1,
        items: sources
            .iter()
            .enumerate()
            .map(|(index, source)| bubble(index, source))
            .collect(),
    }
}

struct Harness {
    snapshot: ViewModelsSnapshot,
    transcript: Transcript,
    caches: Vec<VmCacheSlot>,
    grid: GridSpec,
    context: crate::kit::entry_render_cache::EntryRenderContext,
}

impl Harness {
    fn new(sources: &[String], width: u16) -> Self {
        let grid = GridSpec::grid_for(width);
        Self {
            snapshot: snapshot(sources),
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
            .warm_visible(&self.snapshot, &mut self.caches, scroll_y, height);
        crate::kit::mermaid::drain_preview_jobs_now();
        // 帧发布后重排一次，使围栏范围与高度进入本轮索引。
        let index = self.transcript.prepare(
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
            .warm_visible(&self.snapshot, &mut self.caches, scroll_y, height);
        index
    }

    fn slot_start(&self, index: &SlotIndex, slot: usize) -> usize {
        index.slot_visual_start(slot).unwrap_or(0)
    }
}

fn unique_sources(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let mut source = String::from("graph TD\n");
            for edge in 0..7 {
                source.push_str(&format!("N{index}_{edge}-->M{index}_{edge}\n"));
            }
            source
        })
        .collect()
}

fn count_visible_fences(
    harness: &Harness,
    index: &SlotIndex,
    scroll_y: usize,
    height: usize,
) -> usize {
    let end = scroll_y.saturating_add(height);
    let mut count = 0;
    for slot in index.visible_slots(scroll_y, height) {
        let Some(start) = index.slot_visual_start(slot) else {
            continue;
        };
        let cache = &harness.caches[slot];
        for span in &cache.mermaid_fences {
            let Some(last) = span.logical.end.checked_sub(1) else {
                continue;
            };
            let Some(first_line) = cache.wrap_map.get(span.logical.start) else {
                continue;
            };
            let Some(last_line) = cache.wrap_map.get(last) else {
                continue;
            };
            let fence_start = start.saturating_add(first_line.visual_start);
            let fence_end = start.saturating_add(last_line.visual_end);
            if fence_start < end && fence_end > scroll_y {
                count += 1;
            }
        }
    }
    count
}

#[test]
#[serial_test::serial]
fn viewport_schedules_only_visible_fences_and_reuses_geometry() {
    crate::kit::mermaid::reset_for_test();
    let sources = unique_sources(50);
    let mut harness = Harness::new(&sources, 80);
    let height = 55;

    let mut index = harness.layout(0, height);
    let first = crate::kit::mermaid::layout_count();
    assert_eq!(
        count_visible_fences(&harness, &index, 0, height),
        2,
        "首屏固定两张图可见"
    );
    assert_eq!(first, 2, "首屏只排版两张可见图");

    let scroll_one = harness.slot_start(&index, 1);
    index = harness.layout(scroll_one, height);
    let after_scroll = crate::kit::mermaid::layout_count();
    assert_eq!(
        count_visible_fences(&harness, &index, scroll_one, height),
        2,
        "下滚后仍固定两张图可见"
    );
    assert_eq!(after_scroll - first, 1, "下滚一图只新增一张");

    let _ = harness.layout(0, height);
    let after_back = crate::kit::mermaid::layout_count();
    assert_eq!(after_back, after_scroll, "返回不重排");

    let narrow = GridSpec::grid_for(40);
    harness.grid = narrow;
    index = harness.layout(0, height);
    let after_narrow = crate::kit::mermaid::layout_count();
    assert_eq!(
        count_visible_fences(&harness, &index, 0, height),
        2,
        "40 列仍固定两张图可见"
    );
    assert_eq!(after_narrow - after_back, 2, "40 列重排两张可见图");

    harness.grid = GridSpec::grid_for(80);
    index = harness.layout(0, height);
    let after_restore = crate::kit::mermaid::layout_count();
    assert_eq!(after_restore, after_narrow, "恢复 80 列复用原几何");

    let end_scroll = index.total_visual().saturating_sub(height);
    index = harness.layout(end_scroll, height);
    let after_end = crate::kit::mermaid::layout_count();
    assert_eq!(
        count_visible_fences(&harness, &index, end_scroll, height),
        2,
        "末尾固定两张图可见"
    );
    assert_eq!(after_end - after_restore, 2, "跳到末尾只排版末两图");

    harness
        .snapshot
        .items
        .set(49, bubble(49, "graph TD\nN49-->M49\nN49-->M49b"));
    harness.snapshot.generation += 1;
    let _ = harness.layout(end_scroll, height);
    let after_update = crate::kit::mermaid::layout_count();
    assert_eq!(after_update - after_end, 1, "更新末图只重排一张");
}

#[test]
#[serial_test::serial]
fn identical_templates_share_geometry_across_viewport_moves() {
    crate::kit::mermaid::reset_for_test();
    let sources = vec!["graph TD\nA-->B".to_string(); 50];
    let mut harness = Harness::new(&sources, 80);
    let height = 55;

    let mut index = harness.layout(0, height);
    let first = crate::kit::mermaid::layout_count();
    assert_eq!(first, 1, "相同模板首屏只排版一次");

    let scroll_one = harness.slot_start(&index, 1);
    let _ = harness.layout(scroll_one, height);
    assert_eq!(crate::kit::mermaid::layout_count(), first);

    let _ = harness.layout(0, height);
    assert_eq!(crate::kit::mermaid::layout_count(), first);

    harness.grid = GridSpec::grid_for(40);
    let _ = harness.layout(0, height);
    let after_narrow = crate::kit::mermaid::layout_count();
    assert!(
        crate::kit::mermaid::retained_geometry_bytes() > 0,
        "共享几何纳入内存统计"
    );
    assert_eq!(after_narrow, first + 1, "40 列仍只排版一份模板");

    harness.grid = GridSpec::grid_for(80);
    index = harness.layout(0, height);
    assert_eq!(crate::kit::mermaid::layout_count(), after_narrow);

    let end_scroll = index.total_visual().saturating_sub(height);
    let _ = harness.layout(end_scroll, height);
    assert_eq!(crate::kit::mermaid::layout_count(), after_narrow);

    harness
        .snapshot
        .items
        .set(49, bubble(49, "graph TD\nA-->B\nA-->C"));
    harness.snapshot.generation += 1;
    let _ = harness.layout(end_scroll, height);
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        after_narrow + 1,
        "末图新源码只排版一次"
    );
}

#[test]
#[serial_test::serial]
fn two_fences_in_one_message_are_scheduled_by_line_intersection() {
    crate::kit::mermaid::reset_for_test();
    let text = format!(
        "```mermaid\ngraph TD\nA-->B\n```\n\n{}\n\n```mermaid\ngraph TD\nC-->D\n```",
        "filler paragraph that keeps the second diagram below the viewport. ".repeat(12)
    );
    let mut snapshot = ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::new(),
    };
    snapshot
        .items
        .push_back(TuiRenderUnit::TuiAssistantBubble(Arc::new(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text,
                reasoning: None,
                message_id: Some("two-fences".to_string()),
                content_hash: 4411,
            },
        )));
    let grid = GridSpec::grid_for(80);
    let context = context();
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let height = 12;
    transcript.prepare(
        &snapshot,
        &crate::kit::atoms::TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        0,
        0,
        0,
        height,
        0,
    );
    transcript.warm_visible(&snapshot, &mut caches, 0, height);
    crate::kit::mermaid::drain_preview_jobs_now();
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        1,
        "视口只覆盖第一张图，第二张不排队"
    );

    let second = caches[0].mermaid_fences[1].clone();
    let first_line = caches[0].wrap_map[second.logical.start].visual_start;
    let scroll = first_line.saturating_sub(1);
    let index = transcript.prepare(
        &snapshot,
        &crate::kit::atoms::TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        1,
        scroll,
        height,
        0,
    );
    drop(index);
    transcript.warm_visible(&snapshot, &mut caches, scroll, height);
    crate::kit::mermaid::drain_preview_jobs_now();
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        2,
        "滚到第二张图后只新增它"
    );
}

#[test]
#[serial_test::serial]
fn diagram_height_change_relocates_the_paragraph_under_the_cursor() {
    crate::kit::mermaid::reset_for_test();
    let text = "intro\n\n```mermaid\ngraph TD\nA-->B\n```\n\nreading paragraph";
    let snapshot = ViewModelsSnapshot {
        generation: 1,
        items: im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(Arc::new(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: text.to_string(),
                reasoning: None,
                message_id: Some("reflow".to_string()),
                content_hash: 5511,
            },
        ))]),
    };
    let grid = GridSpec::grid_for(80);
    let context = context();
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let height = 40;
    let first = transcript.prepare(
        &snapshot,
        &crate::kit::atoms::TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        0,
        0,
        0,
        height,
        0,
    );
    transcript.warm_visible(&snapshot, &mut caches, 0, height);
    crate::kit::mermaid::drain_preview_jobs_now();

    let reading_line = (0..first.total_logical())
        .find(|line| {
            first
                .line(0, *line)
                .is_some_and(|line| line.to_string().contains("reading paragraph"))
        })
        .expect("reading paragraph is rendered");
    let scroll = first
        .logical_lookup(reading_line)
        .expect("reading line has a visual position")
        .global_visual_start;

    let index = transcript.prepare(
        &snapshot,
        &crate::kit::atoms::TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        0,
        1,
        scroll,
        height,
        0,
    );
    let relocation = transcript
        .take_reflow_relocation()
        .expect("a visible diagram height change records a reading anchor");
    let target = relocation
        .target_for(false, scroll)
        .expect("browsing keeps the anchored paragraph");
    let lookup = index
        .visual_lookup(target)
        .expect("target visual row exists");
    let relocated = index
        .line(0, lookup.local_logical)
        .expect("target line materializes");
    assert!(
        relocated.to_string().contains("reading paragraph"),
        "relocation must land on the anchored paragraph, got {relocated:?}"
    );
}

#[test]
#[serial_test::serial]
fn diagram_width_uses_body_columns_with_a_100_column_cap() {
    crate::kit::mermaid::reset_for_test();
    let grid = GridSpec::grid_for(120);
    let mut markdown = crate::kit::markdown::MarkdownRenderCache::default();
    let long = TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: format!(
            "```mermaid\ngraph TD\nA-->B\n```\n\n{}",
            "a long reply body that is not brief. ".repeat(10)
        ),
        reasoning: None,
        message_id: Some("wide".to_string()),
        content_hash: 8811,
    }));
    let (_, _, _, _, _, fences) = super::render::vm_to_lines_cached_with_layout(
        &long,
        &grid,
        &mut markdown,
        None,
        false,
        super::grid::ZoneTransition::None,
    );
    assert_eq!(fences[0].width, 100, "正文列宽上限 100");

    let short = TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "```mermaid\ngraph TD\nA-->B\n```".to_string(),
        reasoning: None,
        message_id: Some("brief".to_string()),
        content_hash: 8812,
    }));
    let (_, _, _, _, _, fences) = super::render::vm_to_lines_cached_with_layout(
        &short,
        &grid,
        &mut markdown,
        None,
        false,
        super::grid::ZoneTransition::None,
    );
    assert_eq!(
        fences[0].width,
        grid.process_content_width(),
        "短回复使用过程带收窄后的正文宽度"
    );
}
