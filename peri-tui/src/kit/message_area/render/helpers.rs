use crate::kit::message_area::grid::{Breakpoint, GridSpec, TextLineSource};
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

/// 已完成工具 / subagent 工具行的时长（§6.4）：一位小数秒，`1s` 以内不再出现
/// `ms` 单位；≥1min 回落「Nmin Ms」。
///
/// 四舍五入后即 `0.0s` 的（< 50ms）返回 `None`——「极快」不携带信息，右下角
/// 恒定一个 `0.0s` 只是噪声；该行在 summary 处结束。与格式化同源判断，避免
/// 精度改动后阈值漂移。
pub(super) fn format_completed_duration(ms: u64) -> Option<String> {
    if ms >= 60_000 {
        return Some(format_running_duration(ms));
    }
    let text = format!("{:.1}s", ms as f64 / 1000.0);
    (text != "0.0s").then_some(text)
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
    peri_time::now_wall()
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

/// 过程区首行前缀（§3）：`[outer 空][indent 留空][标记][gap]`——标记（状态
/// 符号）落在 `process_marker_col`（`outer + indent`），正文从与旧形态相同的
/// 列起（col1 的文本竖线投影位让给转场台阶，§6）。
///
/// `indent == 0`（Compact/Narrow）回退 [`first_prefix`]：符号回 accent 列、
/// 不缩进不画台阶（§9，与改动前逐列一致）。
pub(super) fn process_first_prefix(
    grid: &GridSpec,
    symbol: &str,
    style: Style,
) -> Vec<Span<'static>> {
    if grid.indent == 0 {
        return first_prefix(grid, symbol, style);
    }
    vec![
        Span::raw(" "),
        Span::raw(" ".repeat(grid.indent as usize)),
        Span::styled(symbol.to_string(), style),
        Span::raw(" ".repeat(grid.gap as usize)),
    ]
}

/// 过程区续行前缀（§4）：`[outer 空][indent 留空][│][gap]`——标记列与首行
/// 同列（[`GridSpec::process_marker_col`]），正文与首行标签同列（§5）。
///
/// `indent == 0`（Compact/Narrow）回退 [`cont_prefix`]（§9）。
pub(super) fn process_cont_prefix(grid: &GridSpec, color: Color) -> Vec<Span<'static>> {
    if grid.indent == 0 {
        return cont_prefix(grid, color);
    }
    process_first_prefix(grid, "\u{2502}", Style::default().fg(color))
}

// ── 过程区转场台阶（§6）─────────────────────────────────────────────────

/// 进入台阶字形（文本区 → 过程区）：` ╰─╮`——col1 `╰` 接上方文本竖线、
/// col2 `─`、col3 `╮` 接过下方过程区左缘标记列。圆角字形（`╰╮╭╯`）的笔画
/// 端点在格心，与 `│` 的格心笔画严格对齐（斜线字形端点落在格角，单格内
/// 两端各差半格接不齐，已弃）。
pub(super) const STEP_ENTER: &str = " \u{2570}\u{2500}\u{256e}";

/// 退出台阶字形（过程区 → 文本区）：` ╭─╯`（镜像）。
pub(super) const STEP_EXIT: &str = " \u{256d}\u{2500}\u{256f}";

/// 台阶行（宽 4，不含内容）——纯结构 chrome，整行单 span；线色 = 所接文本
/// 左侧线条色（[`text_line_color`]，转场处颜色不断链）；由
/// `vm_to_lines_cached_with_layout` 在转场后的首个 entry 行首插入（§6）。
pub(super) fn step_line(text: &'static str, color: Color) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(color)))
}

/// 台阶线色（§6）：与所接文本的左侧线条同色——user 文本 `text.secondary`、
/// assistant 正文 `accent`、Divider `text.dim`（与其 `── label ──` 同色）；
/// 语义见 [`TextLineSource`]。色值须与各文本渲染器的实际前缀色保持同步
/// （`render/user.rs` / `render.rs` 正文 / `render/system.rs` divider）。
pub(super) fn text_line_color(source: TextLineSource) -> Color {
    let sem = THEME_ATOM.state().read().semantic;
    match source {
        TextLineSource::User => sem.text.secondary,
        TextLineSource::Assistant => sem.accent,
        TextLineSource::Divider => sem.text.dim,
    }
}

/// 台阶行判定（` ╰─╮` / ` ╭─╯`）——Animation 原位刷新（running header 顺延，
/// `render::running_header_index`）与语义复制剥离共用。
pub(super) fn is_step_line(line: &Line<'static>) -> bool {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.content);
    }
    text == STEP_ENTER || text == STEP_EXIT
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

/// 给一行套上过程区续行前缀（缩进 + 标记列竖线 + gap）——短回复正文
/// （`TuiAssistantBubble::is_brief`，并入过程带）与各过程区正文行共用；
/// `indent == 0`（Compact/Narrow）回退 [`prefixed_cont_line`] 基准形态（§9）。
pub(super) fn process_prefixed_cont_line(
    grid: &GridSpec,
    color: Color,
    line: Line<'static>,
) -> Line<'static> {
    let mut spans = process_cont_prefix(grid, color);
    spans.extend(line.spans);
    Line::from(spans)
}

/// duration/metadata 两档放置（§6.4/§11）：
/// - Wide/Standard（content ≥ 60）：右对齐到居中带右缘（`line_width`，跳过滚动条列）——
///   在该带内整行铺满，不再只对齐到 content 列末端；
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
            let line_target = grid.line_width() as usize;
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
/// 保证正文（含前缀）宽度 ≤ 前缀宽 + 正文预算（避免 Paragraph 在视口宽度处
/// 二次折行破坏 wrap_map）。
///
/// 只统计非空白 span 的真实内容宽度——Wide/Standard 断点的右对齐定位填充
/// （`place_meta` 生成的前导空格）不是内容，不参与超宽判定。
///
/// [Why] 上限按行所属区域取：过程区行的前缀比文本区宽 `indent`（§3/§4）且
/// 正文预算同步收窄为 `process_content_width()`（§8 右缘不动），故上限 =
/// `process_prefix_width() + process_content_width()`；Compact/Narrow
/// （`indent == 0`）前缀回退基准形态，保持改动前口径（§9）。
///
/// [Call order] 须在 `place_meta` 追加 gutter 元数据**之前**调用：否则右对齐的
/// duration/计数会成为「最后一个非空白 span」被截成存根（`0.4s` → `0`）——
/// gutter 在正文预算之外，不参与正文裁剪。裁剪后重算 `used` 再放置元数据，
/// 右缘仍落在 `line_width()`。
pub(super) fn fit_summary_to_content(spans: &mut Vec<Span<'static>>, grid: &GridSpec) {
    let content = if grid.indent == 0 {
        grid.content_width()
    } else {
        grid.process_prefix_width() + grid.process_content_width()
    };
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
