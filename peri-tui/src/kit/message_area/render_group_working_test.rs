//! 尾部工作组整行标题扫光：只在 `is_working` 且帧可用时整行换样式，不追加标签；
//! 失败后缀独立红色、先占宽度预算；详情/嵌套（`frame=None`）保持静态 muted。

use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::{Color, Modifier};
use ratatui_kit::ratatui::text::{Line, Span};
use ratatui_kit::ratatui::widgets::{Paragraph, Widget};
use serial_test::serial;

use crate::i18n;
use crate::kit::atoms::{REDUCED_MOTION, TERMINAL_CAPS};
use crate::kit::message_area::grid::{GridSpec, ZoneTransition};
use crate::kit::message_area::render;
use crate::kit::terminal_caps::TerminalCaps;
use crate::kit::tui_render_unit::{FoldState, TuiCollapsedGroup, TuiNoteLevel, TuiRenderUnit};

/// 真实形态的组标题（`Thought` / `Shell` / `Glob` 计数摘要）。
const TITLE: &str = "Thought \u{d7}2 \u{b7} Shell \u{d7}2 \u{b7} Glob";

fn group(is_working: bool, failed_count: u32) -> TuiCollapsedGroup {
    let mut group = TuiCollapsedGroup {
        key: "tool:t1".into(),
        title: TITLE.into(),
        count: 2,
        failed_count,
        view_models: vec![TuiRenderUnit::TuiSystemNote(
            crate::kit::tui_render_unit::TuiSystemNote {
                text: "通知".into(),
                level: TuiNoteLevel::Info,
                content_hash: 1,
            },
        )],
        fold: FoldState::Collapsed,
        is_working,
        content_hash: 0,
    };
    group.recompute_hash();
    group
}

fn rendered(group: &TuiCollapsedGroup, width: u16, frame: Option<u64>) -> Vec<Line<'static>> {
    let grid = GridSpec::grid_for(width);
    let (lines, ..) = render::vm_to_lines_cached_with_layout_gated(
        &TuiRenderUnit::TuiCollapsedGroup(group.clone()),
        &grid,
        &mut Default::default(),
        None,
        false,
        ZoneTransition::None,
        frame,
    );
    lines
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn header_of<'a>(lines: &'a [Line<'static>]) -> &'a Line<'static> {
    lines
        .first()
        .filter(|line| !line.spans.is_empty())
        .expect("组行至少一行")
}

/// 组行标题 span：跳过过程区前缀（outer 空 + 可选 indent + `▸`/`▾` + gap）。
///
/// 无失败后缀时返回的就是整行标题的字符 span（working 逐字符、静态整词）。
fn title_spans<'a>(line: &'a Line<'static>) -> Vec<&'a Span<'static>> {
    let marker = line
        .spans
        .iter()
        .position(|span| matches!(span.content.as_ref(), "\u{25b8}" | "\u{25be}" | "\u{b7}"))
        .expect("组行必须带 ▸/▾ 标记");
    line.spans[marker + 2..].iter().collect()
}

/// 组头前导符号 span（`▸` 折叠 / `▾` 展开）——扫光不覆盖它。
fn marker_span<'a>(line: &'a Line<'static>) -> &'a Span<'static> {
    line.spans
        .iter()
        .find(|span| matches!(span.content.as_ref(), "\u{25b8}" | "\u{25be}"))
        .expect("组头必须带 ▸/▾ 标记")
}

fn span_styles<'a>(
    spans: impl IntoIterator<Item = &'a Span<'static>>,
) -> Vec<(Option<Color>, Modifier)> {
    spans
        .into_iter()
        .map(|span| (span.style.fg, span.style.add_modifier))
        .collect()
}

fn line_styles(lines: &[Line<'static>]) -> Vec<(Option<Color>, Modifier)> {
    lines
        .iter()
        .flat_map(|line| span_styles(line.spans.iter()))
        .collect()
}

fn failed_suffix(count: u32) -> String {
    i18n::tr_args(
        "render-group-failed-count",
        &[("count".to_string(), fluent_bundle::FluentValue::from(count))],
    )
}

struct Guard {
    caps: TerminalCaps,
    reduced: bool,
}

impl Guard {
    fn new() -> Self {
        Self {
            caps: TERMINAL_CAPS.get(),
            reduced: REDUCED_MOTION.get(),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        TERMINAL_CAPS.set(self.caps);
        REDUCED_MOTION.set(self.reduced);
    }
}

#[test]
#[serial]
fn working_group_shimmers_the_whole_title_row() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps::default());
    REDUCED_MOTION.set(false);
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));

    let first_line = header_of(&first);
    let second_line = header_of(&second);
    let text = line_text(first_line);
    assert_eq!(text, line_text(second_line), "两帧文本不变");
    assert!(text.ends_with(TITLE), "标题整行保留：{text}");
    assert!(!text.contains("Working"), "不再追加 Working 标签：{text}");

    let first_spans = title_spans(first_line);
    let second_spans = title_spans(second_line);
    assert_eq!(
        first_spans.len(),
        TITLE.chars().count(),
        "整行标题逐字符扫光"
    );
    assert_ne!(
        span_styles(first_spans.iter().copied()),
        span_styles(second_spans.iter().copied()),
        "不同帧的标题样式必须不同"
    );
    assert!(
        first_spans
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)),
        "truecolor 扫光统一 BOLD"
    );
    assert!(
        first_spans
            .iter()
            .all(|span| matches!(span.style.fg, Some(Color::Rgb(..)))),
        "truecolor 扫光输出 RGB"
    );
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    assert_eq!(
        marker_span(first_line).style.fg,
        Some(sem.text.dim),
        "组前导符号保持静态 dim"
    );
}

#[test]
#[serial]
fn non_working_group_title_is_static_and_frame_stable() {
    let _guard = Guard::new();
    let idle = group(false, 0);
    let first = rendered(&idle, 80, Some(0));
    let second = rendered(&idle, 80, Some(7));
    let text = line_text(header_of(&first));
    assert!(text.ends_with(TITLE));
    assert!(!text.contains("Working"), "非工作组无标签：{text}");
    assert_eq!(
        first.iter().map(line_text).collect::<Vec<_>>(),
        second.iter().map(line_text).collect::<Vec<_>>(),
        "非工作组两帧完全一致"
    );
    assert_eq!(
        line_styles(&first),
        line_styles(&second),
        "非工作组不带帧相位"
    );
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let spans = title_spans(header_of(&first));
    assert_eq!(spans.len(), 1, "静态标题是单个 span");
    assert_eq!(
        spans[0].style.fg,
        Some(sem.text.muted),
        "静态标题保持 muted"
    );
}

#[test]
#[serial]
fn detail_and_nested_renders_keep_the_title_static() {
    let _guard = Guard::new();
    let working = group(true, 0);
    let nested = rendered(&working, 80, None);
    let text = line_text(header_of(&nested));
    assert!(text.ends_with(TITLE), "frame=None 仍显示完整标题：{text}");
    assert!(!text.contains("Working"), "frame=None 不追加标签：{text}");
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let spans = title_spans(header_of(&nested));
    assert_eq!(spans.len(), 1, "详情/嵌套不逐字符扫光");
    assert_eq!(
        spans[0].style.fg,
        Some(sem.text.muted),
        "frame=None 保持静态 muted"
    );
}

#[test]
#[serial]
fn working_group_keeps_the_failed_suffix_independent_and_static() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps::default());
    REDUCED_MOTION.set(false);
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let failing = group(true, 1);
    let failed = failed_suffix(1);

    let first = rendered(&failing, 80, Some(0));
    let second = rendered(&failing, 80, Some(7));
    let first_line = header_of(&first);
    let second_line = header_of(&second);
    let text = line_text(first_line);
    assert!(text.contains(TITLE), "宽屏标题完整：{text}");
    assert!(text.ends_with(&failed), "失败后缀在行尾：{text}");

    let error_span = |line: &Line<'static>| {
        line.spans
            .iter()
            .find(|span| span.content.contains(failed.as_str()))
            .map(|span| {
                (
                    span.content.to_string(),
                    span.style.fg,
                    span.style.add_modifier,
                )
            })
            .expect("失败后缀必须是独立 span")
    };
    let (content, fg, modifier) = error_span(first_line);
    assert_eq!(fg, Some(sem.status.error), "失败后缀保持红色");
    assert_eq!(
        error_span(first_line),
        error_span(second_line),
        "失败后缀不参与扫光：{content}"
    );
    assert_eq!(modifier, Modifier::empty(), "失败后缀不加粗");

    // 标题仍然逐帧扫光（失败后缀不在标题 span 里）。
    let title_only = |line: &Line<'static>| -> Vec<Span<'static>> {
        title_spans(line)
            .into_iter()
            .filter(|span| !span.content.contains(failed.as_str()))
            .cloned()
            .collect()
    };
    assert_ne!(
        span_styles(title_only(first_line).iter()),
        span_styles(title_only(second_line).iter()),
        "带失败后缀时标题仍然扫光"
    );
}

#[test]
#[serial]
fn narrow_width_truncates_the_title_and_keeps_the_failed_suffix() {
    let _guard = Guard::new();
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let failing = group(true, 1);
    let failed = failed_suffix(1);

    // 24 列：content = 18，失败后缀 11 → 标题预算 7 列（含省略号）。
    let narrow = rendered(&failing, 24, Some(3));
    let line = header_of(&narrow);
    let text = line_text(line);
    assert!(!text.contains(TITLE), "整行标题超出预算必须截断：{text}");
    assert!(text.contains('\u{2026}'), "截断带省略号：{text}");
    assert!(text.ends_with(&failed), "失败后缀永不被挤掉：{text}");
    assert!(
        line.width() <= 24,
        "组行不越终端宽度：{} 列 / {text}",
        line.width()
    );
    let last = line.spans.last().expect("至少失败后缀");
    assert_eq!(
        last.style.fg,
        Some(sem.status.error),
        "截断后失败后缀仍为红色"
    );

    // 预算被失败后缀吃穿的极端窄屏：标题至少保留 1 列，不 panic、不吞失败后缀。
    let extreme = rendered(&failing, 12, Some(3));
    let extreme_text = line_text(header_of(&extreme));
    assert!(
        extreme_text.contains('\u{2026}'),
        "标题退化为省略号：{extreme_text}"
    );
    assert!(
        extreme_text.ends_with(&failed),
        "失败后缀仍在：{extreme_text}"
    );
}

#[test]
#[serial]
fn expanded_working_group_shimmers_the_header_row() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps::default());
    REDUCED_MOTION.set(false);
    let mut expanded = group(true, 0);
    expanded.fold = FoldState::Expanded;
    expanded.recompute_hash();
    let first = rendered(&expanded, 80, Some(0));
    let second = rendered(&expanded, 80, Some(7));
    assert!(first.len() > 1, "展开组仍有成员行");
    let header = header_of(&first);
    assert!(line_text(header).contains('\u{25be}'), "展开态组头用 ▾");
    assert!(line_text(header).ends_with(TITLE), "组头整行是标题");
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    assert_eq!(
        marker_span(header).style.fg,
        Some(sem.text.dim),
        "展开态 ▾ 同样保持静态"
    );
    assert_ne!(
        span_styles(title_spans(header).into_iter()),
        span_styles(title_spans(header_of(&second)).into_iter()),
        "展开态组头整行扫光"
    );
}

#[test]
#[serial]
fn working_flag_participates_in_the_content_hash() {
    let plain = group(false, 0);
    let working = group(true, 0);
    assert_ne!(
        plain.content_hash, working.content_hash,
        "is_working 翻转必须改变 hash（缓存重建）"
    );
}

#[test]
#[serial]
fn buffer_cells_carry_the_frame_style() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps::default());
    REDUCED_MOTION.set(false);
    let working = group(true, 0);
    let area = Rect::new(0, 0, 80, 1);
    let paint = |frame: u64| {
        let mut buf = Buffer::empty(area);
        Paragraph::new(rendered(&working, 80, Some(frame))).render(area, &mut buf);
        buf
    };
    let first = paint(0);
    let second = paint(7);
    let differing = (0..80)
        .filter(|&x| first[(x, 0)].fg != second[(x, 0)].fg)
        .count();
    assert!(differing > 0, "两帧在终端 buffer 里的标题前景色必须不同");
    let painted: String = (0..80).map(|x| first[(x, 0)].symbol()).collect();
    assert!(!painted.contains("Working"), "buffer 里不出现 Working");
}

#[test]
#[serial]
fn reduced_motion_renders_the_title_statically() {
    let _guard = Guard::new();
    REDUCED_MOTION.set(true);
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));
    assert!(line_text(header_of(&first)).ends_with(TITLE));
    let spans = title_spans(header_of(&first));
    assert_eq!(spans.len(), 1, "reduced motion 退回整词单 span");
    assert_eq!(
        spans[0].style.fg,
        Some(sem.text.muted),
        "静态基色仍是 muted"
    );
    assert_eq!(
        line_styles(&first),
        line_styles(&second),
        "reduced motion 不运动"
    );
}

#[test]
#[serial]
fn non_truecolor_uses_modifiers_instead_of_rgb() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps {
        truecolor: false,
        ..TerminalCaps::default()
    });
    REDUCED_MOTION.set(false);
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));
    let first_spans = title_spans(header_of(&first));
    let second_spans = title_spans(header_of(&second));
    assert!(
        first_spans
            .iter()
            .all(|span| span.style.fg == Some(sem.text.muted)),
        "非 truecolor 全程基色"
    );
    assert!(
        first_spans
            .iter()
            .map(|span| span.style.add_modifier)
            .ne(second_spans.iter().map(|span| span.style.add_modifier)),
        "强度分级仍随帧变化"
    );
}
