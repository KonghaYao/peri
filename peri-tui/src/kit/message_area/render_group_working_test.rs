//! 尾部工作组 `Working` 标签：只在 `is_working` 且帧可用时出现，扫光按帧换样式；
//! 窄屏先让失败后缀，Working 最后才隐藏。

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

fn group(is_working: bool, failed_count: u32) -> TuiCollapsedGroup {
    let mut group = TuiCollapsedGroup {
        key: "tool:t1".into(),
        title: "Read 2".into(),
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

/// 标签字符 span——组行尾部连续的单字符 span（` · ` 分隔符之后）。
fn label_spans<'a>(line: &'a Line<'static>) -> Vec<&'a Span<'static>> {
    let label = i18n::tr("render-group-working");
    let count = label.chars().count();
    let start = line.spans.len().saturating_sub(count);
    line.spans[start..]
        .iter()
        .filter(|span| span.content.as_ref().chars().count() == 1)
        .collect()
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
fn working_group_shimmers_and_keeps_its_text() {
    let _guard = Guard::new();
    TERMINAL_CAPS.set(TerminalCaps::default());
    REDUCED_MOTION.set(false);
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));
    let label = i18n::tr("render-group-working");

    let first_line = header_of(&first);
    let second_line = header_of(&second);
    assert_eq!(
        line_text(first_line),
        line_text(second_line),
        "两帧文本不变"
    );
    assert!(
        line_text(first_line).contains(&label),
        "组行必须带 Working 标签：{}",
        line_text(first_line)
    );

    let first_styles: Vec<_> = label_spans(first_line)
        .iter()
        .map(|span| (span.style.fg, span.style.add_modifier))
        .collect();
    let second_styles: Vec<_> = label_spans(second_line)
        .iter()
        .map(|span| (span.style.fg, span.style.add_modifier))
        .collect();
    assert_eq!(first_styles.len(), label.chars().count(), "每字符一个 span");
    assert_ne!(first_styles, second_styles, "不同帧的标签样式必须不同");
    assert!(
        label_spans(first_line)
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)),
        "truecolor 扫光统一 BOLD"
    );
    assert!(
        label_spans(first_line)
            .iter()
            .all(|span| matches!(span.style.fg, Some(Color::Rgb(..)))),
        "truecolor 扫光输出 RGB"
    );
}

#[test]
#[serial]
fn non_working_group_has_no_label_and_is_frame_stable() {
    let _guard = Guard::new();
    let idle = group(false, 0);
    let first = rendered(&idle, 80, Some(0));
    let second = rendered(&idle, 80, Some(7));
    let text = line_text(header_of(&first));
    assert!(
        !text.contains(&i18n::tr("render-group-working")),
        "非工作组不画 Working：{text}"
    );
    assert_eq!(
        first.iter().map(line_text).collect::<Vec<_>>(),
        second.iter().map(line_text).collect::<Vec<_>>(),
        "非工作组两帧完全一致"
    );
    let styles = |lines: &[Line<'static>]| -> Vec<(Option<Color>, Modifier)> {
        lines
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|span| (span.style.fg, span.style.add_modifier))
            })
            .collect()
    };
    assert_eq!(styles(&first), styles(&second), "非工作组不带帧相位");
}

#[test]
#[serial]
fn detail_and_nested_renders_skip_the_working_label() {
    let _guard = Guard::new();
    let working = group(true, 0);
    let nested = rendered(&working, 80, None);
    let text = line_text(header_of(&nested));
    assert!(
        !text.contains(&i18n::tr("render-group-working")),
        "frame=None（详情/嵌套）不画 Working：{text}"
    );
}

#[test]
#[serial]
fn narrow_width_keeps_failed_suffix_and_drops_working() {
    let _guard = Guard::new();
    let failing = group(true, 1);
    let failed = i18n::tr_args(
        "render-group-failed-count",
        &[("count".to_string(), fluent_bundle::FluentValue::from(1u32))],
    );
    let working = i18n::tr("render-group-working");
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;

    let wide = rendered(&failing, 40, Some(3));
    let wide_text = line_text(header_of(&wide));
    assert!(wide_text.contains(&working), "40 列仍显示 Working");
    assert!(wide_text.contains(&failed));
    let error_span = wide
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains(&failed))
        .expect("失败后缀必须是独立 span");
    assert_eq!(
        error_span.style.fg,
        Some(sem.status.error),
        "失败后缀保持红色"
    );

    let narrow = rendered(&failing, 24, Some(3));
    let narrow_text = line_text(header_of(&narrow));
    assert!(
        !narrow_text.contains(&working),
        "24 列放不下 Working + 失败后缀时先隐藏 Working：{narrow_text}"
    );
    assert!(
        narrow_text.contains(&failed),
        "失败后缀永不被挤掉：{narrow_text}"
    );
}

#[test]
#[serial]
fn expanded_working_group_keeps_label_on_header() {
    let _guard = Guard::new();
    let mut expanded = group(true, 0);
    expanded.fold = FoldState::Expanded;
    expanded.recompute_hash();
    let lines = rendered(&expanded, 80, Some(0));
    assert!(lines.len() > 1, "展开组仍有成员行");
    assert!(
        line_text(header_of(&lines)).contains(&i18n::tr("render-group-working")),
        "展开态组头仍显示 Working"
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
    let column = (0..80)
        .find(|&x| first[(x, 0)].symbol() == "W")
        .expect("标签首字符必须落在 buffer 里");
    assert_eq!(second[(column, 0)].symbol(), "W");
    assert_ne!(
        first[(column, 0)].fg,
        second[(column, 0)].fg,
        "两帧在终端 buffer 里的标签前景色必须不同"
    );
}

#[test]
#[serial]
fn reduced_motion_renders_the_label_statically() {
    let _guard = Guard::new();
    REDUCED_MOTION.set(true);
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));
    assert!(line_text(header_of(&first)).contains(&i18n::tr("render-group-working")));
    let styles = |lines: &[Line<'static>]| -> Vec<(Option<Color>, Modifier)> {
        lines
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|span| (span.style.fg, span.style.add_modifier))
            })
            .collect()
    };
    assert_eq!(
        styles(&first),
        styles(&second),
        "reduced motion 下标签不运动"
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
    let working = group(true, 0);
    let first = rendered(&working, 80, Some(0));
    let second = rendered(&working, 80, Some(7));
    let first_spans = label_spans(header_of(&first));
    let second_spans = label_spans(header_of(&second));
    let sem = peri_theme::atoms::THEME_ATOM.state().read().semantic;
    assert!(
        first_spans
            .iter()
            .all(|span| span.style.fg == Some(sem.status.running)),
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
