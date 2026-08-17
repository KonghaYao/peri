use crate::kit::message_area::grid::{Breakpoint, GridSpec};
use crate::kit::terminal_caps::SymbolSet;
use crate::kit::tui_render_unit::TuiDiffBlock;
use crate::truncate::truncate_by_width;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Color, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

// ── 工具卡片辅助函数（内联自 view_render/tool_card.rs）─────────────────

pub(super) fn compact_output_lines(text: &str, max_lines: usize, max_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(|line| truncate_by_width(line, max_width))
        .collect();

    let total = text.lines().filter(|line| !line.trim().is_empty()).count();
    if total > max_lines {
        lines.push(format!("\u{2026} {} more lines", total - max_lines));
    }

    lines
}

pub(super) fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

/// 已完成工具的时长格式（§6.4 `37ms` / `4.2s`；≥1min 回落秒/分格式）。
pub(super) fn format_completed_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format_running_duration(ms)
    }
}

pub(super) fn diff_change_summary(diff: &TuiDiffBlock) -> Option<String> {
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(diff);
    if adds == 0 && dels == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if adds > 0 {
        parts.push(format!("+{}", adds));
    }
    if dels > 0 {
        parts.push(format!("-{}", dels));
    }
    Some(parts.join(" \u{b7} "))
}

// ── 符号与语义色辅助 ─────────────────────────────────────────────────────

/// 当前终端能力的符号集（§4.1 降级表）。
pub(super) fn sym() -> SymbolSet {
    crate::kit::terminal_caps::symbols(&crate::kit::atoms::TERMINAL_CAPS.state().read())
}

/// 状态符号 + 状态色：running/success/error 三元组。
/// running 符号为动态 braille 动画帧（§8.2），返回 String。
pub(super) fn status_symbol_and_color(
    is_running: bool,
    is_error: bool,
    sem: &peri_theme::semantic::SemanticTokens,
) -> (String, Color) {
    if is_error {
        (sym().error.to_string(), sem.status.error)
    } else if is_running {
        (running_symbol(), sem.status.running)
    } else {
        (sym().success.to_string(), sem.status.success)
    }
}

/// 动画 tick：100ms 粒度的壁钟帧序号（与 mod.rs 缓存重建的 anim_frame 同源）。
/// 渲染层每次重绘取当前帧——running 行由缓存按帧强制重建驱动动画推进。
fn anim_tick() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 / 100)
        .unwrap_or(0)
}

/// running 状态符号：unicode 终端用单字符宽的 braille 方块动画帧，
/// ASCII 降级为 `*`（§4.1 降级表）。
fn running_symbol() -> String {
    let caps = *crate::kit::atoms::TERMINAL_CAPS.state().read();
    if caps.unicode {
        crate::components::spinner::animation::braille_frame(anim_tick()).to_string()
    } else {
        "*".to_string()
    }
}

// ── 网格前缀（§3.1）─────────────────────────────────────────────────────

/// 块首行前缀：`[outer 空][accent 符号][gap]`。
/// Narrow 断点：accent 符号退化为 dim bullet（§11）。
pub(super) fn first_prefix(grid: &GridSpec, symbol: &str, style: Style) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    if grid.is_narrow() {
        let sem = THEME_ATOM.state().read().semantic;
        spans.push(Span::styled("\u{b7}", Style::default().fg(sem.text.dim)));
    } else {
        spans.push(Span::styled(symbol.to_string(), style));
    }
    spans.push(Span::raw(" ".repeat(grid.gap as usize)));
    spans
}

/// 续行前缀：`[outer 空][语义色竖线][gap]`——竖线颜色由调用方按消息类型传入。
pub(super) fn cont_prefix(grid: &GridSpec, color: Color) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled("\u{2502}", Style::default().fg(color)),
        Span::raw(" ".repeat(grid.gap as usize)),
    ]
}

/// 给一行套上续行前缀；Markdown 段落空行也保留 accent 竖线。
pub(super) fn prefixed_cont_line(
    grid: &GridSpec,
    color: Color,
    line: Line<'static>,
) -> Line<'static> {
    let mut spans = cont_prefix(grid, color);
    spans.extend(line.spans);
    Line::from(spans)
}

/// duration/metadata 两档放置（§6.4/§11）：
/// - Wide/Standard（content ≥ 60）：右对齐到消息区右缘（`term_width - 1`，
///   跳过滚动条列）——整行铺满，不再只对齐到 content 列末端；
///   右对齐放不下时回退「紧跟 summary」（保底不丢失，Standard 长 summary 场景）；
/// - Compact/Narrow（< 60）：隐藏非关键 duration；
/// - 返回 None 表示该元数据不渲染。
pub(super) fn place_meta(
    grid: &GridSpec,
    used: usize,
    meta: &str,
    style: Style,
) -> Option<Vec<Span<'static>>> {
    let content = grid.content_width();
    let w = meta.width();
    match grid.bp {
        Breakpoint::Wide | Breakpoint::Standard => {
            let line_target = grid.term_width.saturating_sub(1) as usize;
            if used + 2 + w <= line_target {
                Some(vec![
                    Span::raw(" ".repeat(line_target.saturating_sub(used + w))),
                    Span::styled(meta.to_string(), style),
                ])
            } else if used + 2 + w <= content {
                Some(vec![Span::styled(format!("  {meta}"), style)])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 用 `truncate_by_width` 截断超宽行内最后一个可变 span（summary），
/// 保证整行宽度 ≤ content（避免 Paragraph 在视口宽度处二次折行破坏 wrap_map）。
///
/// 只统计非空白 span 的真实内容宽度——Wide/Standard 断点的右对齐定位填充
/// （`place_meta` 生成的前导空格）不是内容，不参与超宽判定（否则
/// 右对齐的 duration/计数会被误截成 `…`）。
pub(super) fn fit_summary_to_content(spans: &mut Vec<Span<'static>>, grid: &GridSpec) {
    let content = grid.content_width();
    let content_total: usize = spans
        .iter()
        .filter(|s| !s.content.trim().is_empty())
        .map(|s| s.content.width())
        .sum();
    if content_total <= content {
        return;
    }
    let overflow = content_total - content;
    for span in spans.iter_mut().rev() {
        if span.content.trim().is_empty() {
            continue;
        }
        let w = span.content.width();
        if w > 0 {
            let keep = w.saturating_sub(overflow);
            if keep > 0 {
                span.content = truncate_by_width(&span.content, keep).into();
            }
            return;
        }
    }
}
