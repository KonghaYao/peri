//! 第 3 部分验收：真实视口相交调度、几何复用与阅读锚点。

use std::sync::Arc;

use super::grid::GridSpec;
use super::selection::SlotIndex;
use super::transcript::Transcript;
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::ViewModelsSnapshot;
use crate::kit::entry_render_cache::EntryRenderOptions;
use crate::kit::entry_render_cache::EntrySurface;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use peri_theme::atoms::THEME_ATOM;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;

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

    /// 只重排一次（peek）；`warm` 决定是否同时调度可见围栏。
    fn prepare(&mut self, scroll_y: usize, height: usize, warm: bool) -> Arc<SlotIndex> {
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
        );
        if warm {
            self.warm(scroll_y, height);
        }
        index
    }

    fn warm(&mut self, scroll_y: usize, height: usize) {
        self.transcript
            .warm_visible(&self.snapshot, &mut self.caches, scroll_y, height);
    }

    fn layout(&mut self, scroll_y: usize, height: usize) -> Arc<SlotIndex> {
        self.prepare(scroll_y, height, true);
        crate::kit::mermaid::drain_preview_jobs_now();
        // 帧发布后重排一次，使围栏范围与高度进入本轮索引。
        self.prepare(scroll_y, height, true)
    }

    fn slot_start(&self, index: &SlotIndex, slot: usize) -> usize {
        index.slot_visual_start(slot).unwrap_or(0)
    }
}

/// 槽位当前全部逻辑行的文本（不经真实 Buffer）。
fn slot_text(cache: &VmCacheSlot) -> String {
    let lines = cache.lines.as_ref().expect("slot lines");
    (0..cache.wrap_map.len())
        .filter_map(|index| lines.line(index))
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 把槽位逻辑行画进真实 Buffer，断言「屏幕上」也没有源码。
fn slot_buffer(cache: &VmCacheSlot, width: u16) -> Buffer {
    let lines = cache.lines.as_ref().expect("slot lines");
    let rows: Vec<_> = (0..cache.wrap_map.len())
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

fn buffer_row(buffer: &Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|column| buffer[(column, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
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
fn mermaid_action_line_is_the_clickable_last_line_of_the_fence() {
    crate::kit::mermaid::reset_for_test();
    let sources = vec!["graph TD\nA-->B".to_string()];
    let mut harness = Harness::new(&sources, 80);
    let index = harness.layout(0, 40);
    assert!(index.slot_visual_start(0).is_some(), "首屏应能定位消息");
    let span = harness.caches[0]
        .mermaid_fences
        .first()
        .expect("围栏已渲染")
        .clone();
    let action = span.logical.end.checked_sub(1).expect("动作行");
    assert!(action >= span.logical.start);
    let lines = harness.caches[0].lines.as_ref().expect("slot lines");
    let text = lines.get(action).expect("动作行可见").to_string();
    assert!(
        text.contains("View diagram") || text.contains("查看图"),
        "最后一行应是可点击的查看图入口: {text}"
    );
    assert!(
        harness.caches[0].wrap_map.get(action).is_some(),
        "动作行必须有视觉行映射，点击才能换算屏幕坐标"
    );
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
    assert_eq!(
        first, 3,
        "首帧无可用帧时围栏只有占位与动作行，第三张图也进入视口并各排一次"
    );

    let scroll_one = harness.slot_start(&index, 1);
    index = harness.layout(scroll_one, height);
    let after_scroll = crate::kit::mermaid::layout_count();
    assert_eq!(
        count_visible_fences(&harness, &index, scroll_one, height),
        2,
        "下滚后仍固定两张图可见"
    );
    assert_eq!(
        after_scroll - first,
        0,
        "首帧已覆盖三张可见图，下滚一图不新增排版"
    );

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
    let (_, _, _, _, _, fences, _) = super::render::vm_to_lines_with_options(
        &long,
        &grid,
        &mut markdown,
        None,
        EntryRenderOptions::isolated(super::grid::ZoneTransition::None, false),
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
    let (_, _, _, _, _, fences, _) = super::render::vm_to_lines_with_options(
        &short,
        &grid,
        &mut markdown,
        None,
        EntryRenderOptions::isolated(super::grid::ZoneTransition::None, false),
    );
    assert_eq!(
        fences[0].width,
        grid.process_content_width(),
        "短回复使用过程带收窄后的正文宽度"
    );
}

/// 流式 chunk 到达时消息区只 `prepare`（peek）、尚未 `warm`（observe）：
/// 这一帧必须继续显示上一张可信帧，屏幕与逻辑行都不能出现围栏源码。
#[test]
#[serial_test::serial]
fn streaming_chunk_has_no_raw_source_before_observe() {
    crate::kit::mermaid::reset_for_test();
    let height = 40;
    let mut harness = Harness::new(&["graph TD\nN0-->M0\nN1-->M1".to_string()], 80);
    let _ = harness.layout(0, height);
    let layouts = crate::kit::mermaid::layout_count();
    assert!(layouts >= 1, "首帧应已排版");

    // chunk 到达：围栏仍未闭合，刷新 VM 后只重排一次（peek）。
    harness.snapshot.items.set(
        0,
        TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "```mermaid\ngraph TD\nN0-->M0\nN1-->M1\nN2--".to_string(),
            reasoning: None,
            message_id: Some("mermaid-0".to_string()),
            content_hash: 9001,
        })),
    );
    harness.snapshot.generation += 1;
    let _ = harness.prepare(0, height, false);

    let cache = &harness.caches[0];
    let rendered = slot_text(cache);
    assert!(
        !rendered.contains("graph TD") && !rendered.contains("N0-->M0"),
        "chunk 到达帧不得内联围栏源码: {rendered}"
    );
    assert!(
        !rendered.contains("N2--"),
        "chunk 到达帧不得内联未确认尾部: {rendered}"
    );
    assert!(
        rendered.contains('┌') || rendered.contains('╭'),
        "上一张可信帧应继续显示: {rendered}"
    );
    assert!(
        rendered.contains("View diagram") || rendered.contains("查看图"),
        "动作行必须保留: {rendered}"
    );
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        layouts,
        "peek 不得排队或重排几何"
    );

    let buffer = slot_buffer(cache, harness.grid.line_width());
    let painted: Vec<String> = (0..buffer.area.height)
        .map(|row| buffer_row(&buffer, row))
        .collect();
    let painted = painted.join("\n");
    assert!(
        !painted.contains("graph TD") && !painted.contains("N2--"),
        "真实 Buffer 不得画出源码: {painted}"
    );
    assert!(
        painted.contains('┌') || painted.contains('╭'),
        "真实 Buffer 应保留可信帧: {painted}"
    );
}

/// 列宽变化后的第一帧同样先渲染后观察：复用旧帧，不闪回源码。
#[test]
#[serial_test::serial]
fn resize_reuses_the_previous_frame_until_the_new_one_arrives() {
    crate::kit::mermaid::reset_for_test();
    let height = 40;
    let mut harness = Harness::new(&["graph TD\nN0-->M0\nN1-->M1".to_string()], 80);
    let _ = harness.layout(0, height);
    let baseline = crate::kit::mermaid::layout_count();
    assert!(baseline >= 1, "首帧应已排版");

    harness.grid = GridSpec::grid_for(40);
    let _ = harness.prepare(0, height, false);
    let rendered = slot_text(&harness.caches[0]);
    assert!(
        !rendered.contains("graph TD"),
        "resize 首帧不得内联源码: {rendered}"
    );
    assert!(
        rendered.contains('┌') || rendered.contains('╭'),
        "旧列宽的帧应继续显示: {rendered}"
    );
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        baseline,
        "复用旧帧时不得排队新排版"
    );

    harness.warm(0, height);
    crate::kit::mermaid::drain_preview_jobs_now();
    let _ = harness.prepare(0, height, true);
    assert_eq!(
        crate::kit::mermaid::layout_count(),
        baseline + 1,
        "40 列只新增一份几何"
    );
    let resized = slot_text(&harness.caches[0]);
    assert!(
        resized.contains('┌') || resized.contains('╭'),
        "新帧应替换旧帧: {resized}"
    );
    assert!(
        !resized.contains("graph TD"),
        "resize 后仍不得内联源码: {resized}"
    );
}
