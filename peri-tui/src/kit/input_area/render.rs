use unicode_width::UnicodeWidthStr;

use ratatui_kit::ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::kit::message_area::grid::GridSpec;
use peri_theme::atoms::THEME_ATOM;

/// §10 queued 队列在 composer 上方最多显示的行数，超出显示 `· · ·`。
pub(super) const QUEUE_VISIBLE_MAX: usize = 5;

fn input_tokens() -> peri_theme::component::InputTokens {
    THEME_ATOM.state().read().component.input
}

/// §10 composer 边框：title_top 右侧 session title；title_bottom 左侧
/// `@ N files` + 右侧资源线（`footer_right`：CPU% · MEM · ctx，原状态栏迁移）。
/// 窄屏（§11）逐级隐藏：`show_top=false`（h<12）隐藏 title_top 整行；
/// `show_bottom=false`（h<8）隐藏 title_bottom。
/// `max_width` = composer 区域宽度（`use_previous_size`，resize 后次帧收敛）。
#[allow(clippy::too_many_arguments)] // 标题位/可见性/宽度参数同属一个边框语义，拆分反增复杂度
pub(super) fn build_composer_block(
    loading: bool,
    session_title: &str,
    files_label: Option<&str>,
    footer_right: Option<Line<'static>>,
    show_top: bool,
    show_bottom: bool,
    max_width: u16,
) -> Block<'static> {
    let tokens = input_tokens();
    let sem = THEME_ATOM.state().read().semantic;
    let border_color = if loading {
        tokens.border_loading
    } else {
        tokens.border
    };

    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    if show_top && !session_title.is_empty() {
        let title_width = session_title.width().min(32) + 2;
        if title_width <= usize::from(max_width) {
            block = block.title_top(build_session_title_line(session_title).right_aligned());
        }
    }
    if show_bottom {
        // 左侧附件计数 / 右侧资源线（CPU·MEM·ctx，muted + 资源阈值色）
        if let Some(f) = files_label {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {f} "),
                Style::default().fg(sem.text.muted),
            )));
        }
        if let Some(line) = footer_right {
            block = block.title_bottom(line.right_aligned());
        }
    }
    block
}

/// 资源线分隔符：` · `（muted，与 composer footer 其余文本同色系）。
pub(super) fn footer_separator(color: Color) -> Span<'static> {
    Span::styled(" · ", Style::default().fg(color))
}

/// §10 queued 队列行：`· {text}`（queued 符号 + muted），每行按 composer
/// 文本宽度截断；超过 [`QUEUE_VISIBLE_MAX`] 条时末行 `· · ·`。
pub(super) fn build_queue_lines(
    items: &[String],
    has_more: bool,
    max_width: usize,
) -> Vec<Line<'static>> {
    if items.is_empty() {
        return Vec::new();
    }
    let sem = THEME_ATOM.state().read().semantic;
    let muted = Style::default().fg(sem.text.muted);
    let sym = crate::kit::terminal_caps::symbols(&crate::kit::atoms::TERMINAL_CAPS.state().read());
    let mut lines = Vec::with_capacity(items.len() + usize::from(has_more));
    for text in items {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", sym.queued), muted),
            Span::styled(crate::truncate::truncate_by_width(text, max_width), muted),
        ]));
    }
    if has_more {
        lines.push(Line::from(vec![Span::styled(
            format!("{} {} {}", sym.queued, sym.queued, sym.queued),
            muted,
        )]));
    }
    lines
}

/// §10 对齐：composer 正文起点 = prompt 前缀宽度（outer1 + accent1 + gap，
/// 与 transcript `first_prefix_width` 一致）+ 右预留 2 列。gap=1 → 5；
/// gap=2 → 6。
pub(super) fn prompt_and_border_width(grid: GridSpec) -> u16 {
    (2 + grid.gap) + 2
}

/// 会话标题标签：hash 稳定底色 + 按亮度反色前景 + BOLD。
///
/// 同一标题经确定性 hash 后始终命中同一底色，不同标题大概率不同色；
/// 底色来自主题 `input.session_title_palette`，遵循"主题不硬编码颜色"约束。
pub(super) fn build_session_title_line(title: &str) -> Line<'static> {
    let palette = input_tokens().session_title_palette;
    let bg = palette[stable_hash(title) as usize % palette.len()];
    Line::from(Span::styled(
        format!(" {} ", truncate_title_to_width(title, 32)),
        Style::default()
            .bg(bg)
            .fg(readable_fg(bg))
            .add_modifier(Modifier::BOLD),
    ))
}

/// FNV-1a 64 位确定性 hash——不依赖 `std` DefaultHasher 的随机 seed，
/// 保证同一标题在跨进程 / 跨会话场景下颜色稳定。
pub(super) fn stable_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 按终端显示宽度截断标题（CJK 双宽字符按 2 列计），超长补省略号。
///
/// 委托共享 helper `crate::truncate::truncate_by_width`（历史实现已迁移，语义不变）。
pub(super) fn truncate_title_to_width(s: &str, max_width: usize) -> String {
    crate::truncate::truncate_by_width(s, max_width)
}

/// 根据底色亮度选择黑白对比前景（保证可读性的"反色"效果）。
pub(super) fn readable_fg(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
            if luminance > 140.0 {
                Color::Black
            } else {
                Color::White
            }
        }
        _ => Color::White,
    }
}

/// §10/§3.1 对齐：prompt 前缀宽度 = outer(1) + accent(1) + gap ——与 transcript
/// content 起点（`first_prefix_width`）一致。composer 无左右 border
/// （`Borders::TOP|BOTTOM`），正文起点即前缀宽度：gap=1 → `" ❯ "`（3 列），
/// gap=2 → `" ❯  "`（4 列）。续行前缀同宽（accent 位置留空）。
pub(super) fn build_composer_lines(
    editor_lines: Vec<Line<'static>>,
    loading: bool,
    grid: GridSpec,
) -> Vec<Line<'static>> {
    let tokens = input_tokens();
    let mut lines = Vec::with_capacity(editor_lines.len().max(1));
    let prompt_style = Style::default()
        .fg(if loading {
            tokens.prompt_loading
        } else {
            tokens.prompt
        })
        .add_modifier(Modifier::BOLD);

    let prompt_prefix = format!(" \u{276f}{}", " ".repeat(grid.gap as usize));
    let cont_prefix = " ".repeat(grid.first_prefix_width());

    if editor_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(prompt_prefix, prompt_style),
            Span::raw(""),
        ]));
        return lines;
    }

    for (index, line) in editor_lines.into_iter().enumerate() {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        if index == 0 {
            spans.push(Span::styled(prompt_prefix.clone(), prompt_style));
        } else {
            spans.push(Span::styled(
                cont_prefix.clone(),
                Style::default().fg(tokens.continuation),
            ));
        }
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

pub(super) fn popup_height(item_count: usize) -> u16 {
    (item_count.max(1) as u16 + 2).min(THEME_ATOM.state().read().component.popup.inline_height)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_multiline_with_cursor_for_themed(
    text: &str,
    cursor: usize,
    selection_range: Option<(usize, usize)>,
    placeholder: Option<&str>,
    max_width: usize,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<ratatui::text::Line<'static>> {
    let tokens = input_tokens();
    let cursor_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::BOLD);
    let selection_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::DIM);
    let placeholder_style = Style::default().fg(tokens.placeholder);
    let default_style = Style::default().bg(Color::Reset);
    crate::components::textarea::render_multiline_with_cursor(
        text,
        cursor,
        cursor_style,
        selection_range,
        selection_style,
        placeholder,
        placeholder_style,
        default_style,
        max_width,
        viewport_height,
        loading,
        show_cursor,
    )
}
