//! vm_to_lines + 各变体渲染函数 + 辅助函数。
//!
//! [Slice 3] 统一水平网格（§3.1）：所有 entry 共享 `outer + accent + gap + content`
//! 前缀结构——块首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][dim 竖线][gap]`；
//! Narrow 断点 accent 符号退化为 bullet（§11）。正文全部从 content 列起点对齐。

use crate::i18n;
use crate::kit::message_area::grid::{Breakpoint, GridSpec};
use crate::kit::terminal_caps::SymbolSet;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAskUserBlock, TuiCollapsedGroup, TuiDiffBlock, TuiDivider,
    TuiHunkLineKind, TuiNoteLevel, TuiReasoningBlock, TuiRenderUnit, TuiSubAgentGroup,
    TuiSystemNote, TuiTodoChangeKind, TuiTodoPresentation, TuiTodoSummary, TuiToolCard,
    TuiToolPresentation,
};
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// 用户 prompt 正文最多展示的视觉行数（§6.1），超出显示 `… +N lines`。
const USER_BODY_MAX_LINES: usize = 6;
/// Expanded reasoning 正文的最大行数（防超长推理块撑爆视口）。
const REASONING_BODY_MAX_LINES: usize = 100;
/// 工具输出展开的最大行数（与历史 expanded 行为一致）。
const TOOL_OUTPUT_MAX_LINES: usize = 4;
/// 子 agent 名称定宽（§6.7 前缀宽度稳定——running 摘要更新不改前缀列）。
const SUBAGENT_NAME_WIDTH: usize = 16;

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
fn sym() -> SymbolSet {
    crate::kit::terminal_caps::symbols(&crate::kit::atoms::TERMINAL_CAPS.state().read())
}

/// 状态符号 + 状态色：running/success/error 三元组。
fn status_symbol_and_color(
    is_running: bool,
    is_error: bool,
    sem: &peri_theme::semantic::SemanticTokens,
) -> (&'static str, Color) {
    if is_error {
        (sym().error, sem.status.error)
    } else if is_running {
        (sym().running, sem.status.running)
    } else {
        (sym().success, sem.status.success)
    }
}

// ── 网格前缀（§3.1）─────────────────────────────────────────────────────

/// 块首行前缀：`[outer 空][accent 符号][gap]`。
/// Narrow 断点：accent 符号退化为 dim bullet（§11）。
fn first_prefix(grid: &GridSpec, symbol: &str, style: Style) -> Vec<Span<'static>> {
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

/// 续行前缀：`[outer 空][角色色竖线][gap]`——竖线按消息角色区分颜色：
/// user/assistant/reasoning/tool 取 `semantic.accents` 角色色，其余场景
/// （system note / ask user / subagent / divider）由调用方传 `text.dim`。
fn cont_prefix(grid: &GridSpec, color: Color) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled("\u{2502}", Style::default().fg(color)),
        Span::raw(" ".repeat(grid.gap as usize)),
    ]
}

/// 给一行套上续行前缀；Markdown 段落空行也保留 accent 竖线。
fn prefixed_cont_line(grid: &GridSpec, color: Color, line: Line<'static>) -> Line<'static> {
    let mut spans = cont_prefix(grid, color);
    spans.extend(line.spans);
    Line::from(spans)
}

/// duration/metadata 三档放置（§6.4/§11）：
/// - Wide（content ≥ 100）：右对齐到 content 列末端（行总宽 = 前缀 + content）；
/// - Standard（60–99）：紧跟 summary；
/// - Compact/Narrow（< 60）：隐藏非关键 duration；
/// - 返回 None 表示该元数据不渲染。
fn place_meta(
    grid: &GridSpec,
    used: usize,
    meta: &str,
    style: Style,
) -> Option<Vec<Span<'static>>> {
    let content = grid.content_width();
    let w = meta.width();
    match grid.bp {
        Breakpoint::Wide => {
            let line_target = grid.first_prefix_width() + content;
            if used + 2 + w <= content {
                Some(vec![
                    Span::raw(" ".repeat(line_target.saturating_sub(used + w))),
                    Span::styled(meta.to_string(), style),
                ])
            } else {
                None
            }
        }
        Breakpoint::Standard => {
            if used + 2 + w <= content {
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
/// 只统计非空白 span 的真实内容宽度——Wide 断点的右对齐定位填充
/// （`place_meta` 生成的前导空格）不是内容，不参与超宽判定（否则
/// 右对齐的 duration/计数会被误截成 `…`）。
fn fit_summary_to_content(spans: &mut Vec<Span<'static>>, grid: &GridSpec) {
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

// ── vm_to_lines：TuiRenderUnit → Vec<Line<'static>> ───────────────────────

/// md 复制按钮信息：按钮行在 VM lines 内的逻辑索引 + 行内列范围（相对消息区）。
/// 由 [`vm_to_lines_cached`] 在渲染按钮行时返回，供 MessageArea 构建屏幕点击区域。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CopyButtonInfo {
    /// 按钮行在 VM lines 中的逻辑索引。
    pub(super) logical_idx: usize,
    /// 按钮文本起始列（行内，相对消息区 x=0）。
    pub(super) x_start: u16,
    /// 按钮文本结束列（不含）。
    pub(super) x_end: u16,
}

/// md 复制按钮的最小内容长度（字符数，`chars().count()` 口径——与复制反馈
/// `mark_copy_message` 同一计数方式）。短消息直接选中复制即可，不渲染按钮，
/// 避免每条消息都出现一行按钮造成视觉噪音。
const MD_COPY_MIN_CHARS: usize = 400;

/// 生成 md 复制按钮行（AssistantBubble 内容末尾）。
///
/// 布局：行尾右对齐的 ` Copy `——整个反色块（含左右 1 空格）即按钮视觉，
/// 前导无样式空格把按钮推到 content 列右缘（继承消息区背景，视觉上不留痕迹）；
/// 点击区域与反色块完全重合（[x_start, x_end) = 按钮块本身，不含前导空格）。
/// 宽度不足（按钮行会被折行）时返回 None——调用方不渲染按钮行，避免
/// 点击区域与实际渲染位置错位（同 footer keepgoing 的 m4 fix）。
fn copy_button_line(grid: &GridSpec) -> Option<(Line<'static>, u16, u16)> {
    let semantic = THEME_ATOM.state().read().semantic;
    let btn_text = i18n::tr("msg-copy-md");
    let btn_span = Span::styled(
        format!(" {btn_text} "),
        // 反色：accent 前景 + REVERSED → 终端
        Style::default()
            .fg(semantic.accent)
            .add_modifier(Modifier::REVERSED),
    );
    let btn_width = btn_span.width();
    // 按钮行宽度 = 前缀（outer+accent+gap）+ content——按钮右对齐在 content 列右缘。
    let line_width = grid.first_prefix_width() + grid.content_width();
    if btn_width > line_width {
        return None;
    }
    // 右对齐：前导无样式空格（默认样式，渲染时继承消息区背景）占满按钮左侧。
    let line = Line::from(vec![
        Span::raw(" ".repeat(line_width - btn_width)),
        btn_span,
    ]);
    let x_start = (line_width - btn_width) as u16;
    let x_end = line_width as u16;
    Some((line, x_start, x_end))
}

/// 将单个 TuiRenderUnit 变体转换为渲染行。
/// 使用 kit::markdown 进行 markdown 解析（无缓存，每次全量）。
///
/// 仅测试断言使用（[`render_test`]）——生产路径统一走 [`vm_to_lines_cached`]。
#[cfg(test)]
pub(super) fn vm_to_lines(vm: &TuiRenderUnit, grid: &GridSpec) -> Vec<Line<'static>> {
    vm_to_lines_cached(
        vm,
        grid,
        &mut crate::kit::markdown::MarkdownRenderCache::default(),
        false,
    )
    .0
}

/// §9 语义复制（D3）：按 VM 变体从渲染行提取「语义文本」——复制内容而非
/// 屏幕像素。供 `extract_visual_range`（selection.rs）在复制时调用（事件时点，
/// 非渲染路径）；md 复制按钮路径（复制原始 markdown）不动。
///
/// 传入**已渲染行**（复制路径持有 VmCacheSlot 的 Arc<Vec<Line>>）而非重新
/// 渲染——旧实现每行新建 MarkdownRenderCache 全量重渲染 VM（N 行选区 = N 次
/// 全量 markdown 解析，§15 线性度违背）；渲染行与缓存行同源，结果等价。
///
/// 变体分派：
/// - 普通行（user/reasoning/assistant md/tool 输出/system/divider 等）：剥离
///   `outer + accent + gap` 前缀列（§3.1 网格）——`line_to_plain_text` 无符号；
/// - tool header 行：`{Verb} {summary}{suffix}`（label + summary + 完成后缀，
///   无符号、无 duration——§9「Read header 复制 path，Bash header 复制
///   command」）；
/// - Bash 展开 `$ cmd` 行：保留 `$ {command}`（§9）；
/// - diff 行：剥离行号 gutter 列，保留 `+`/`-` patch 标记与正文（§9）；
/// - code block 行：再剥离 `│ ` gutter（现状无语言标签行/行号——§9 已确认）。
///
/// 未命中变体回退前缀剥离结果（保留现有语义）。
pub(crate) fn semantic_line_text(
    vm: &TuiRenderUnit,
    local_idx: usize,
    line: &ratatui_kit::ratatui::text::Line<'static>,
    grid: &GridSpec,
) -> Option<String> {
    let plain = crate::kit::text_selection::line_to_plain_text(line);
    let stripped = strip_visual_prefix(line, &plain, grid, local_idx);
    match vm {
        TuiRenderUnit::TuiToolCard(card) => {
            if local_idx == 0 {
                // header 行：label + summary + suffix（§9）。
                return Some(tool_header_semantic(card));
            }
            // Bash 展开 `$ cmd` 行（§9 保留 command）。
            if let Some(rest) = stripped.strip_prefix("$ ") {
                return Some(format!("$ {rest}"));
            }
            // diff 行：数字行号列 + 符号 → 剥行号保留 patch 标记（§9）。
            if let Some(sem) = strip_diff_gutter(&stripped) {
                return Some(sem);
            }
            Some(stripped)
        }
        TuiRenderUnit::TuiAssistantBubble(_) => {
            // code block 行：再剥 `│ ` gutter（语言标签/行号现状无——§9）。
            if let Some(rest) = stripped.strip_prefix("\u{2502} ") {
                return Some(rest.to_string());
            }
            Some(stripped)
        }
        _ => Some(stripped),
    }
}

/// §9 tool header 行语义：`{Verb} {summary}{suffix}`——无符号、无 duration。
/// Bash 展开态 summary 移到 `$ ` 行（render_generic_tool_card_lines 口径），
/// header 语义只留 label；suffix 复用 `completed_header_suffix`（Read `— N
/// lines` / Glob/Grep `— N matches` / Edit/Write `· +N −M`——§6.4 口径，
/// Edit/Write 不重复输出含路径的摘要文本）。
fn tool_header_semantic(data: &TuiToolCard) -> String {
    let mut text = crate::kit::tool_display::format_tool_name(&data.tool_name);
    let bash_expanded = data.tool_name == "Bash"
        && !data.is_running
        && !data.is_error
        && data.fold == FoldState::Expanded;
    if !bash_expanded && !data.input_summary.is_empty() {
        text.push(' ');
        text.push_str(&data.input_summary);
    }
    let suffix = completed_header_suffix(data);
    if !suffix.is_empty() {
        text.push_str(&suffix);
    }
    text
}

/// 从渲染行剥离网格前缀列（§3.1：`outer + accent + gap`），返回内容列文本。
///
/// 前缀由行首 spans 结构确认（渲染层约定：首行 `[outer " ", 符号, gap " "]`
/// = `first_prefix`，续行 `[outer " ", │, gap " "]` = `cont_prefix`）；
/// 结构不符（无前缀行，如 md 复制按钮行的前导空格）不剥离——兜底保留原样。
fn strip_visual_prefix(
    line: &ratatui_kit::ratatui::text::Line<'static>,
    plain: &str,
    grid: &GridSpec,
    local_idx: usize,
) -> String {
    if plain.is_empty() {
        return String::new();
    }
    let expect = if local_idx == 0 {
        grid.first_prefix_width()
    } else {
        grid.cont_prefix_width()
    };
    let mut w = 0usize;
    let mut byte_skip = 0usize;
    let mut hit = false;
    for span in &line.spans {
        let sw = span.content.width();
        if sw == 0 {
            byte_skip += span.content.len();
            continue;
        }
        if w + sw > expect {
            break; // 结构不符（前缀在 expect 内断掉）——视为无前缀行
        }
        w += sw;
        byte_skip += span.content.len();
        if w == expect {
            hit = true;
            break;
        }
    }
    if hit {
        plain[byte_skip..].to_string()
    } else {
        plain.to_string()
    }
}

/// §9 diff 行语义：`{行号列} {符号} {正文}` → `{符号} {正文}`（剥行号 gutter，
/// 保留 `+`/`-` patch 标记）；context 行（符号为空格）→ 纯正文。
///
/// 模式 `^\s*\d+ [+ -] `（gutter 右对齐可能含前导空格；符号位后必须跟空格）
/// ——普通输出行（如 `42  foo`）不匹配（符号后无空格）→ `None` 回退原样。
fn strip_diff_gutter(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    // 前导空格（gutter 右对齐填充）
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    // 数字行号
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    // 分隔空格（恰好一个）
    if bytes.get(i) != Some(&b' ') {
        return None;
    }
    let sym_i = i + 1;
    let sym = *bytes.get(sym_i)?;
    if !matches!(sym, b'+' | b'-' | b' ') {
        return None;
    }
    if bytes.get(sym_i + 1) != Some(&b' ') {
        return None;
    }
    let body = &text[sym_i + 2..];
    match sym {
        b'+' => Some(format!("+ {body}")),
        b'-' => Some(format!("- {body}")),
        _ => Some(body.to_string()),
    }
}

/// 与 [`vm_to_lines`] 同逻辑，但接受 markdown 渲染缓存以支持增量续跑。///
/// [Phase 2] 流式期间 text 末尾追加 token 时，前缀 blocks（已闭合的 paragraph /
/// list item / code block）完全不变——cache 通过文本前缀比较复用上次处理到
/// stable_state 的累积状态，仅处理新增 block。
///
/// `render_copy_button` 为 true 时（顶层 MessageArea），在 AssistantBubble 的
/// markdown 内容末尾追加一行 md 复制按钮，并返回按钮的布局信息（供点击检测）。
/// 嵌套渲染（历史 SubAgentGroup 递归 / subagent 详情面板）传 false。
///
/// 返回三元组 `(lines, copy_button, interaction_layout)`——第三项仅 pending
/// 的 interaction block（§6.8）有值（选项行布局，供视口 post-pass 高亮与
/// 点击热区）；其余变体恒 None。
pub(crate) fn vm_to_lines_cached(
    vm: &TuiRenderUnit,
    grid: &GridSpec,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
    render_copy_button: bool,
) -> (
    Vec<Line<'static>>,
    Option<CopyButtonInfo>,
    Option<InteractionLayout>,
) {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let mut lines: Vec<Line<'static>> = Vec::new();

            // §3.2 垂直节奏：user 与 assistant 正文块上下各保留 1 空行。
            // reasoning 是过程 entry，不自行增加空行；若后接正文，正文前导空行
            // 负责分隔。空 bubble（无 text 无 reasoning）仍返回 0 行。
            if data.text.is_empty() && data.reasoning.is_none() {
                return (lines, None, None);
            }

            // 推理块（§6.3）——视觉独立 entry
            if let Some(ref reasoning) = data.reasoning {
                lines.extend(render_reasoning_block(reasoning, grid));
                // 块尾不加空行（running/completed 一致）——与工具卡片紧凑布局
                // 对齐；正文紧随其后，由 md 渲染自身节奏负责分段。
            }

            // Markdown 正文——上下各 1 空行；wrap 在 content 列宽，行级再套统一前缀。
            if !data.text.is_empty() {
                lines.push(Line::default());
                let theme_guard = peri_theme::atoms::THEME_ATOM.state();
                let theme = theme_guard.read();
                let md_text_fg = theme.component.markdown.text;
                // 正文续行竖线用 assistant 角色色（§4 accents 表）
                let line_color = theme.semantic.accents.assistant;
                let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
                let palette_guard = palette_state.read();
                let segments = crate::kit::markdown::parse_markdown_cached(
                    &data.text,
                    grid.content_width(),
                    *palette_guard,
                    md_text_fg,
                    md_cache,
                );
                for (seg_idx, seg) in segments.into_iter().enumerate() {
                    // segment 之间加空行（表格 ↔ 文本边界）
                    if seg_idx > 0 && !lines.last().is_some_and(|l| l.spans.is_empty()) {
                        lines.push(prefixed_cont_line(grid, line_color, Line::default()));
                    }
                    match seg {
                        crate::kit::markdown::MarkdownSegment::Text(seg_lines) => {
                            for line in seg_lines {
                                lines.push(prefixed_cont_line(grid, line_color, line));
                            }
                        }
                        crate::kit::markdown::MarkdownSegment::Table(data) => {
                            let mut table_theme =
                                ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                            // 行文字使用终端默认色，而非纯白
                            table_theme.row_style = Style::default();
                            let table_lines = crate::kit::markdown::table_data_to_lines(
                                &data,
                                &table_theme,
                                grid.content_width(),
                            );
                            for tl in table_lines {
                                lines.push(prefixed_cont_line(grid, line_color, tl));
                            }
                        }
                    }
                }
            }

            // §6.2 完成时长 meta（G-Tokens 仅 duration）：冻结值在正文末行尾部
            // 三档放置（Wide 右对齐 / Standard 紧跟 / Compact/Narrow 隐藏），
            // 不独占一行。空正文（纯 reasoning 块）无放置点 → 跳过。
            if let Some(duration_ms) = data.duration_ms {
                let sem = THEME_ATOM.state().read().semantic;
                let meta = format_completed_duration(duration_ms);
                let meta_style = Style::default().fg(sem.text.dim);
                if let Some(last_non_empty) = lines.iter_mut().rev().find(|l| !l.spans.is_empty()) {
                    let used: usize = last_non_empty.spans.iter().map(|s| s.content.width()).sum();
                    if let Some(meta_spans) = place_meta(grid, used, &meta, meta_style) {
                        last_non_empty.spans.extend(meta_spans);
                    }
                }
            }

            // md 复制按钮：独立于 markdown 渲染，追加在内容末尾。
            // 仅当内容超过 MD_COPY_MIN_CHARS 字符（chars().count() 口径）才渲染。
            let copy_button = if render_copy_button && data.text.chars().count() > MD_COPY_MIN_CHARS
            {
                if let Some((btn_line, x_start, x_end)) = copy_button_line(grid) {
                    // 不插空行：按钮紧贴内容末行，视觉上更紧凑
                    let logical_idx = lines.len();
                    lines.push(btn_line);
                    Some(CopyButtonInfo {
                        logical_idx,
                        x_start,
                        x_end,
                    })
                } else {
                    None
                }
            } else {
                None
            };

            if !data.text.is_empty() {
                lines.push(Line::default());
            }

            (lines, copy_button, None)
        }
        TuiRenderUnit::TuiUserBubble(data) => {
            if let Some(ref info) = data.reminder {
                return (render_reminder_condensed(info, grid), None, None);
            }
            (render_user_bubble_lines(data, grid), None, None)
        }
        TuiRenderUnit::TuiToolCard(data) => (render_tool_card_lines(data, grid), None, None),
        TuiRenderUnit::TuiSystemNote(data) => (render_system_note_lines(data, grid), None, None),
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            (render_subagent_group_lines(data, grid), None, None)
        }
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            (render_collapsed_group_lines(data, grid), None, None)
        }
        TuiRenderUnit::TuiDivider(data) => (render_divider_lines(data, grid), None, None),
        TuiRenderUnit::TuiAskUserBlock(data) => {
            let (lines, layout) = render_ask_user_block_lines(data, grid);
            (lines, None, layout)
        }
        TuiRenderUnit::TuiTodoSummary(data) => (render_todo_summary_lines(data, grid), None, None),
    }
}

// ── 各变体渲染函数（Slice 3：统一网格 + 无气泡 + 垂直节奏）──────────────

/// §6.1 User prompt：去全宽 bg 与 `❯`；无 role label（`You`），正文直接开始。
///
/// - 首行 1 个空行（§3.2 turn 节拍），正文行 `[│][gap]` 与其余 entry 同起点。
/// - 保留用户换行；长 prompt 最多 `USER_BODY_MAX_LINES` 个视觉行，
///   超出显示 `… +N lines`（§6.1）。
/// - slash command / `@mention` 局部强调（accent.user）。
fn render_user_bubble_lines(
    data: &crate::kit::tui_render_unit::TuiUserBubble,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    // 空文本 user（rewind/重放路径的 thinking 回传消息建模为 user role，
    // 提取文本为空）→ 渲染 0 行——不产生 turn 节拍空行，避免 thinking
    // 底下出现悬空空行（§3.2 节拍只属于真实 user prompt）。
    if data.text.is_empty() {
        return Vec::new();
    }
    // §3.2：新 user prompt 前保留 1 个空行（turn 节拍）。
    let mut lines: Vec<Line<'static>> = vec![Line::from("")];

    // 正文：保留用户换行；每行按 display width 折行成「视觉行」（§6.1 口径），
    // 最多 USER_BODY_MAX_LINES 个视觉行，超出显示 `… +N lines`（§12：grapheme
    // + display width，CJK/emoji 不被从中间切开）。
    let visual_lines: Vec<String> = data
        .text
        .lines()
        .flat_map(|l| crate::truncate::wrap_by_width(l, grid.content_width()))
        .collect();
    let total = visual_lines.len();
    let shown = total.min(USER_BODY_MAX_LINES);
    for raw in &visual_lines[..shown] {
        lines.push(Line::from({
            let mut spans = cont_prefix(grid, sem.accents.user);
            spans.extend(emphasize_user_line(raw, grid, &sem));
            spans
        }));
    }
    if total > shown {
        let more = i18n::tr_args(
            "render-more-lines",
            &[(
                "count".to_string(),
                FluentValue::from((total - shown) as u64),
            )],
        );
        let mut spans = cont_prefix(grid, sem.accents.user);
        spans.push(Span::styled(more, Style::default().fg(sem.text.dim)));
        lines.push(Line::from(spans));
    }
    // §3.2：user 尾部 1 空行（turn 节拍对称）——分隔后续 thinking/tool；
    // assistant 正文仍由自身前导空行建立正文块边界。
    lines.push(Line::from(""));
    lines
}

/// 用户正文行的局部强调（§6.1）：`@mention` / `/command` token 用 accent.user，
/// 其余正文 text.primary。
fn emphasize_user_line(
    raw: &str,
    grid: &GridSpec,
    sem: &peri_theme::semantic::SemanticTokens,
) -> Vec<Span<'static>> {
    let primary = Style::default().fg(sem.text.primary);
    let emphasis = Style::default().fg(sem.accents.user);
    let budget = grid.content_width();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut width = 0usize;
    let mut truncated = false;
    for token in raw.split_inclusive(' ') {
        let w = token.trim_end().width().max(1);
        if width + w > budget {
            truncated = true;
            break;
        }
        let is_emph = token.starts_with('@') || token.starts_with('/');
        spans.push(Span::styled(
            token.to_string(),
            if is_emph { emphasis } else { primary },
        ));
        width += w;
    }
    if truncated {
        // 截断行尾部补省略号（预算 1 列）
        if width >= budget {
            spans.pop();
        }
        spans.push(Span::styled("\u{2026}", primary));
    }
    spans
}

/// §6.3 Reasoning 三态视觉。
///
/// - Running/Preview：`[◐][gap]Thinking…`（bold, status.running）+ elapsed（三档放置）
///   + 最近 ≤4 行 tail（muted+italic，无 italic → dim；`│` 续行前缀与 md 正文一致）。
/// - Completed/Collapsed：单行 `[▸][gap]Thought for 12s · 14 lines`。
/// - Completed/Expanded：`[▾][gap]Thought for 12s · 14 lines` + 正文（≤100 行）。
/// - 空 reasoning 仍显示 `Thinking…`（不出现空白 block）。
fn render_reasoning_block(reasoning: &TuiReasoningBlock, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let caps = *crate::kit::atoms::TERMINAL_CAPS.state().read();
    // 正文样式：muted + italic；无 italic 能力 → dim（§4.1/§12）。
    let body_style = if caps.italic {
        Style::default()
            .fg(sem.text.muted)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(sem.text.dim)
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let line_count = reasoning
        .text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();

    if reasoning.is_running {
        // Running（fold=Preview 或用户覆盖 Expanded）：◐ Thinking… + elapsed + tail。
        // [Fix §6.3] 用户手动折叠（Collapsed）→ 仅活动状态行，不渲染 tail——
        // 「隐藏 reasoning 只影响 body；活动状态行仍需可见」（§7 running 默认
        // Preview，Collapsed 只能来自用户覆盖——Space 切换必须有视觉反馈）。
        let mut spans = first_prefix(grid, sym().running, Style::default().fg(sem.status.running));
        // 对齐工具卡片语言（§6.4 硬编码英文口径，避免中英混杂）；信息层级
        // 低于工具——label 用 muted（工具 label 为 primary+bold），活动感由
        // ◐（running 色）承担。
        spans.push(Span::styled(
            "Thinking…",
            Style::default().fg(sem.text.muted),
        ));
        let used: usize = spans.iter().map(|s| s.content.width()).sum();
        let elapsed = format!("{}s", reasoning.duration_secs());
        if let Some(meta) = place_meta(grid, used, &elapsed, Style::default().fg(sem.text.dim)) {
            spans.extend(meta);
        }
        lines.push(Line::from(spans));

        let tail_max = match reasoning.fold {
            FoldState::Expanded => REASONING_BODY_MAX_LINES,
            FoldState::Preview => 4, // §6.3：最近 2–4 个视觉行 tail
            FoldState::Collapsed => 0,
        };
        for tail in reasoning
            .text
            .lines()
            .rev()
            .take(tail_max)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if tail.trim().is_empty() {
                continue;
            }
            let mut spans = cont_prefix(grid, sem.accents.reasoning);
            spans.push(Span::styled(
                truncate_by_width(tail, grid.content_width()),
                body_style,
            ));
            lines.push(Line::from(spans));
        }
    } else {
        // Completed：折叠单行 / 展开含正文。
        // [Fix LOW-6] Preview（§7 表 completed 只定义 Collapsed；Space 可从
        // Collapsed 切到 Preview）映射到单行折叠视觉——`▾` 但无正文的
        // 「假展开」箭头会误导（正文只在 Expanded 渲染）。
        let symbol = if reasoning.fold == FoldState::Expanded {
            sym().expanded
        } else {
            sym().collapsed
        };
        // 对齐工具卡片（§6.4 硬编码英文后缀口径）：`Thought for 12s · 26 lines`
        // / `Thought · 26 lines`（时长不可得降级）。语言对齐工具区域（避免中英
        // 混杂）；信息层级低于工具——整行 dim（icon + 摘要；工具主干为
        // primary/syntax 色，dim 与其最低后缀级持平）。
        let summary = if reasoning.duration_ms.is_some() {
            format!(
                "Thought for {}s · {} lines",
                reasoning.duration_secs(),
                line_count
            )
        } else {
            format!("Thought · {} lines", line_count)
        };
        let mut spans = first_prefix(grid, symbol, Style::default().fg(sem.text.dim));
        spans.push(Span::styled(summary, Style::default().fg(sem.text.dim)));
        lines.push(Line::from(spans));

        if reasoning.fold == FoldState::Expanded {
            for body_line in reasoning.text.lines().take(REASONING_BODY_MAX_LINES) {
                if body_line.trim().is_empty() {
                    continue;
                }
                let mut spans = cont_prefix(grid, sem.accents.reasoning);
                spans.push(Span::styled(
                    truncate_by_width(body_line, grid.content_width()),
                    body_style,
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// 用户消息内的 system-reminder（§6.1/§6.6）：按来源型 system event 渲染——
/// 首行 `[!][gap]{来源 label}`，续行 `[│][gap]{摘要}`。
fn render_reminder_condensed(
    info: &crate::kit::tui_render_unit::ReminderInfo,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = vec![Line::from({
        let mut spans = first_prefix(grid, sym().warning, Style::default().fg(sem.status.warning));
        spans.push(Span::styled(
            info.reminder_type.label(),
            Style::default()
                .fg(sem.text.muted)
                .add_modifier(Modifier::ITALIC),
        ));
        spans
    })];
    if !info.summary.is_empty() {
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&info.summary, grid.content_width()),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    lines
}

/// §6.4 Tool activity：compact 行，不是重型 card。
///
/// - 首行 `[◐|✓|×][gap]{Verb}`（单 bold 锚点）`{summary}` + 后缀 + duration 三档。
/// - summary 着色：Bash → syntax.command；Read/Write/Edit → syntax.path；其余 muted。
/// - error：× + status.error 符号色 + 明确错误词（`— Failed`），正文不整块染红。
/// - running（Preview）：仅活动行（运行中无 output）；completed：折叠单行 /
///   展开输出（≤4 行）；Bash 展开显示 `$ command` + 分隔线 + 输出。
fn render_tool_card_lines(data: &TuiToolCard, grid: &GridSpec) -> Vec<Line<'static>> {
    match &data.presentation {
        TuiToolPresentation::Skill(skill) => {
            return render_skill_tool_card_lines(data, &skill.name, grid);
        }
        TuiToolPresentation::Todo(todo) => {
            return render_todo_tool_card_lines(data, todo, grid);
        }
        TuiToolPresentation::Generic => {}
    }

    render_generic_tool_card_lines(data, grid)
}

/// 语义卡（Skill/Todo）保持专属展示，套用统一网格前缀。
fn with_prefix_lines(
    grid: &GridSpec,
    symbol: &str,
    symbol_style: Style,
    content: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = Vec::with_capacity(content.len());
    for (i, line) in content.into_iter().enumerate() {
        let spans = if i == 0 {
            let mut spans = first_prefix(grid, symbol, symbol_style);
            spans.extend(line.spans);
            spans
        } else {
            let mut spans = cont_prefix(grid, sem.accents.tool);
            spans.extend(line.spans);
            spans
        };
        lines.push(Line::from(spans));
    }
    lines
}

fn render_skill_tool_card_lines(
    data: &TuiToolCard,
    name: &str,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content_width = grid.content_width().saturating_sub(1).max(1);
    let (symbol, indicator_color) = status_symbol_and_color(data.is_running, data.is_error, &sem);

    if data.is_running {
        let title = format!("Skill ({})", name);
        return with_prefix_lines(
            grid,
            symbol,
            Style::default().fg(indicator_color),
            vec![
                Line::from(vec![Span::styled(
                    truncate_by_width(&title, content_width),
                    Style::default()
                        .fg(sem.text.primary)
                        .add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    i18n::tr("msg-status-loading"),
                    Style::default().fg(sem.text.muted),
                )]),
            ],
        );
    }

    // [F3] 状态符号走降级表（§12：Unicode 能力不足时 × → x / ✓ → +），
    // 不硬编码原始 UTF-8。
    let status = if data.is_error {
        sym().error
    } else {
        sym().success
    };
    let title = format!("Skill ({}) - {}", name, status);
    with_prefix_lines(
        grid,
        symbol,
        Style::default().fg(indicator_color),
        vec![Line::from(vec![Span::styled(
            truncate_by_width(&title, content_width),
            Style::default()
                .fg(sem.text.primary)
                .add_modifier(Modifier::BOLD),
        )])],
    )
}

fn render_todo_tool_card_lines(
    data: &TuiToolCard,
    todo: &TuiTodoPresentation,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content_width = grid.content_width().saturating_sub(1).max(1);
    let (symbol, indicator_color) = status_symbol_and_color(data.is_running, data.is_error, &sem);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if data.is_error {
        let title = i18n::tr("tool-todo-failed");
        lines.push(Line::from(vec![Span::styled(
            truncate_by_width(&title, content_width),
            Style::default()
                .fg(sem.text.primary)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            truncate_by_width(&data.output_summary, content_width),
            Style::default().fg(sem.text.muted),
        )]));
    } else {
        let title = format!("TodoUpdate ({}/{})", todo.completed_count, todo.total_count);
        lines.push(Line::from(vec![Span::styled(
            truncate_by_width(&title, content_width),
            Style::default()
                .fg(sem.text.primary)
                .add_modifier(Modifier::BOLD),
        )]));

        for change in &todo.changes {
            // [F3] 图标走符号降级表（§12）：✓→success；▶/↻/✎ 无 §4.1 语义
            // 条目，按辅助字形降级（ASCII：> / ~ / *）。
            let s = sym();
            let (icon, color) = match change.kind {
                TuiTodoChangeKind::Completed => (s.success, sem.status.success),
                TuiTodoChangeKind::Added => ("+", sem.status.success),
                TuiTodoChangeKind::Removed => ("-", sem.text.muted),
                TuiTodoChangeKind::Started => (s.todo_started, sem.status.success),
                TuiTodoChangeKind::Reopened => (s.todo_reopened, sem.status.success),
                TuiTodoChangeKind::ActiveFormUpdated => (s.todo_edited, sem.status.success),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    truncate_by_width(&change.content, content_width),
                    Style::default().fg(sem.text.muted),
                ),
            ]));
        }
    }
    with_prefix_lines(grid, symbol, Style::default().fg(indicator_color), lines)
}

fn render_generic_tool_card_lines(data: &TuiToolCard, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let display_name = crate::kit::tool_display::format_tool_name(&data.tool_name);
    let (symbol, symbol_color) = status_symbol_and_color(data.is_running, data.is_error, &sem);
    // 展开态（completed+Expanded / error=Expanded）首行符号换 ▾（§6.4 示例）。
    let symbol = if !data.is_running && !data.is_error && data.fold == FoldState::Expanded {
        sym().expanded
    } else {
        symbol
    };

    let mut spans = first_prefix(grid, symbol, Style::default().fg(symbol_color));
    // label：每行最多一个 bold 主锚点
    spans.push(Span::styled(
        display_name.clone(),
        Style::default()
            .fg(sem.text.primary)
            .add_modifier(Modifier::BOLD),
    ));

    // summary 主对象：Bash→command（syntax.command）；Read/Write/Edit→path（syntax.path）
    let summary_color = match data.tool_name.as_str() {
        "Bash" => sem.syntax.command,
        "Read" | "Write" | "Edit" => sem.syntax.path,
        _ => sem.text.muted,
    };
    // Bash 展开时 command 移到 `$ ` 行——首行只留 label + duration。
    let bash_expanded = data.tool_name == "Bash"
        && !data.is_running
        && !data.is_error
        && data.fold == FoldState::Expanded;
    let show_summary = !bash_expanded;

    // 固定部件宽度（label + 错误词后缀）预算 summary 宽度
    let error_word = if data.is_error {
        format!(" \u{2014} {}", i18n::tr("msg-status-failed"))
    } else {
        String::new()
    };
    let label_width = display_name.width() + error_word.width() + 2;
    if show_summary {
        let suffix = completed_header_suffix(data);
        let budget = content.saturating_sub(label_width + suffix.width() + 2);
        let summary = truncate_by_width(&data.input_summary, budget.max(1));
        if !summary.is_empty() {
            spans.push(Span::styled(
                format!(" {summary}"),
                Style::default().fg(summary_color),
            ));
        }
        if !suffix.is_empty() {
            spans.push(Span::styled(suffix, Style::default().fg(sem.text.dim)));
        }
    }

    // duration 三档（§6.4）：running 秒 / completed ms·s 冻结值
    let duration_text = if data.is_running {
        data.running_duration_ms.map(format_running_duration)
    } else {
        data.completed_duration_ms.map(format_completed_duration)
    };
    if !error_word.is_empty() {
        spans.push(Span::styled(
            error_word,
            Style::default()
                .fg(sem.status.error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    if let Some(meta) = duration_text
        .as_deref()
        .and_then(|d| place_meta(grid, used, d, Style::default().fg(sem.text.dim)))
    {
        spans.extend(meta);
    }
    fit_summary_to_content(&mut spans, grid);
    let mut lines = vec![Line::from(spans)];

    // Bash 展开：`$ command`（syntax.command）+ 分隔线 + 输出
    if bash_expanded {
        let mut cmd_spans = cont_prefix(grid, sem.accents.tool);
        cmd_spans.push(Span::styled(
            format!(
                "$ {}",
                truncate_by_width(&data.input_summary, content.saturating_sub(2))
            ),
            Style::default().fg(sem.syntax.command),
        ));
        lines.push(Line::from(cmd_spans));
        lines.push(divider_fill_line(grid));
    }

    // 输出摘要（§6.4）：error 摘要拆行 + error 色（§9.2）；展开态 ≤4 行。
    // [G-Diff] 含 diff 的 Edit/Write 展开体由 render_diff_lines 展示（§6.5）——
    // diff 文本就是 output_summary 本体，原始行直接显示会与 hunk 渲染重复。
    let show_output = if data.is_error {
        true
    } else {
        data.fold == FoldState::Expanded
    };
    let has_diff = data.diff.is_some();
    if show_output && !has_diff && !data.output_summary.is_empty() {
        // [Fix F6 §11] Compact/Narrow 断点：tool 展开体最多 2 行（§11
        // 「tool summary 最多 2 行」）——标准断点保持 4 行上限。
        let max_lines = if matches!(grid.bp, Breakpoint::Compact | Breakpoint::Narrow) {
            2
        } else {
            TOOL_OUTPUT_MAX_LINES
        };
        // [§9.2 错误输出] `Tool execution failed: X - Error: …` 按 ` - Error: `
        // 分隔符拆成两行（首行工具名、次行错误详情），整块 error 色——
        // 错误详情不再与工具名挤在同一行。
        let output = if data.is_error {
            data.output_summary.replacen(" - Error: ", "\n- Error: ", 1)
        } else {
            data.output_summary.clone()
        };
        let output_color = if data.is_error {
            sem.status.error
        } else {
            sem.text.muted
        };
        for out_line in compact_output_lines(&output, max_lines, content) {
            let mut spans = cont_prefix(grid, sem.accents.tool);
            spans.push(Span::styled(out_line, Style::default().fg(output_color)));
            lines.push(Line::from(spans));
        }
    }

    // [G-Diff] §6.5 diff 展开体（展开态；header `path +N −M` + hunk 行渲染）。
    if show_output && let Some(ref diff) = data.diff {
        lines.extend(render_diff_lines(diff, grid));
    }

    lines
}

/// §6.5 Diff 展开体（G-Diff）：
///
/// ```text
/// src/render.rs  +3 −1              ← header：path（syntax.path）+ 计数（dim）
/// @@ -1,3 +1,4 @@                   ← hunk 头（dim）
///  10   context line                ← 行号 gutter（dim） + 符号 + 正文
///  11 - old line                    ← Del：error fg；Add：success fg
///  11 + new line
/// … +2 more lines                   ← 截断指示（本 hunk 或后续 hunk 的剩余 change）
/// ```
///
/// - insert/delete 同时使用 `+`/`-`、foreground 与低对比背景（surface.sunken），
///   不只靠红绿（§6.5）；
/// - 行号 gutter 使用 dim（不参与正文复制——§9 由语义复制层剥离）；
/// - 窄屏（Compact/Narrow）先隐藏行号列，再硬截断代码（不软换行，§6.5）；
/// - 只展示首个 hunk（§6.5「默认展示首个 hunk」）；剩余 change 行计数进
///   `… +N more lines`。
fn render_diff_lines(diff: &TuiDiffBlock, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    // 行号列宽度（4 位右对齐 + 1 空格）——窄屏先隐藏（§6.5）。
    let show_gutter = !matches!(grid.bp, Breakpoint::Compact | Breakpoint::Narrow);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // header：`{path} {+N} {−M}`（path syntax.path；计数 dim）。
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(diff);
    let mut count_parts = Vec::new();
    if adds > 0 {
        count_parts.push(format!("+{adds}"));
    }
    if dels > 0 {
        count_parts.push(format!("\u{2212}{dels}"));
    }
    let count_text = count_parts.join(" ");
    let mut spans = cont_prefix(grid, sem.accents.tool);
    if !diff.path.is_empty() {
        spans.push(Span::styled(
            truncate_by_width(
                &diff.path,
                content.saturating_sub(count_text.width().min(content)),
            ),
            Style::default().fg(sem.syntax.path),
        ));
    }
    if !count_text.is_empty() {
        if !diff.path.is_empty() {
            spans.push(Span::styled(" ", Style::default()));
        }
        spans.push(Span::styled(
            truncate_by_width(&count_text, content),
            Style::default().fg(sem.text.dim),
        ));
    }
    lines.push(Line::from(spans));

    // 首个 hunk（§6.5「默认展示首个 hunk」）。
    let Some(hunk) = diff.hunks.first() else {
        return lines;
    };
    // hunk 头（dim）。
    let mut hunk_header = cont_prefix(grid, sem.accents.tool);
    hunk_header.push(Span::styled(
        truncate_by_width(
            &format!("@@ {} {} @@", hunk.old_range, hunk.new_range),
            content,
        ),
        Style::default().fg(sem.text.dim),
    ));
    lines.push(Line::from(hunk_header));

    // change / context 行：gutter（dim）+ `+`/`-` fg + 低对比 bg（sunken）。
    let diff_bg = sem.surface.sunken;
    let gutter_width = 4usize;
    for l in &hunk.lines {
        let (gutter, symbol, fg) = match l.kind {
            TuiHunkLineKind::Add => (l.new_no, "+", sem.status.success),
            TuiHunkLineKind::Del => (l.old_no, "-", sem.status.error),
            TuiHunkLineKind::Context => (l.old_no, " ", sem.text.muted),
        };
        let mut spans = cont_prefix(grid, sem.accents.tool);
        if show_gutter {
            let no = gutter
                .map(|n| format!("{n:>gutter_width$}"))
                .unwrap_or_else(|| " ".repeat(gutter_width));
            spans.push(Span::styled(
                no,
                Style::default().fg(sem.text.dim).bg(diff_bg),
            ));
            spans.push(Span::styled(" ", Style::default().bg(diff_bg)));
        }
        let symbol_span = Span::styled(symbol, Style::default().fg(fg).bg(diff_bg));
        spans.push(symbol_span);
        spans.push(Span::styled(" ", Style::default().bg(diff_bg)));
        // 代码不软换行——超宽硬截断（§6.5「窄屏先隐行号再裁切代码」）。
        let used = spans.iter().map(|s| s.content.width()).sum::<usize>();
        let budget = content.saturating_sub(used).max(1);
        let body = truncate_by_width(&l.text, budget);
        spans.push(Span::styled(
            body,
            Style::default().fg(sem.text.primary).bg(diff_bg),
        ));
        lines.push(Line::from(spans));
    }

    // 截断指示：本 hunk 截断 + 后续 hunk 的剩余 change 行（§6.5 `… +N more lines`）。
    let remaining = hunk.truncated_lines + diff.more_change_lines;
    if remaining > 0 {
        let mut more = cont_prefix(grid, sem.accents.tool);
        more.push(Span::styled(
            format!("\u{2026} +{remaining} more lines"),
            Style::default().fg(sem.text.dim),
        ));
        lines.push(Line::from(more));
    }

    lines
}

/// 完成工具头行后缀（历史行为保留）：Read `— N lines`；Glob/Grep `— N matches`；
/// Edit/Write `· +N −M`（只保留 diff 计数——摘要文本含路径，与 header 的
/// `input_summary` 重复，不再拼接）。错误态不加后缀（§6.4）。
fn completed_header_suffix(data: &TuiToolCard) -> String {
    if data.is_running || data.is_error || data.output_summary.is_empty() {
        return String::new();
    }
    match data.tool_name.as_str() {
        "Read" => {
            let total_lines = data
                .output_summary
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            format!(" \u{2014} {} lines", total_lines)
        }
        "Glob" | "Grep" => {
            let total = data
                .output_summary
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            format!(" \u{2014} {} matches", total)
        }
        "Edit" | "Write" => data
            .diff
            .as_ref()
            .and_then(diff_change_summary)
            .map(|s| format!(" \u{b7} {}", s))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// 内容列铺满的 dim 分隔线（`[outer ][│][gap][───…]`）——Bash 展开体分隔用
/// （竖线随工具卡片取 tool 角色色）。
fn divider_fill_line(grid: &GridSpec) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut spans = cont_prefix(grid, sem.accents.tool);
    spans.push(Span::styled(
        "\u{2500}".repeat(grid.content_width()),
        Style::default().fg(sem.text.dim),
    ));
    Line::from(spans)
}

/// §6.6 System event：普通事件单行 divider；warning/error 用符号 + accent。
///
/// - Info：`── {来源文本} ──…`（divider 填满 content 列）。
/// - Warning/Error：`[!|×][gap]{文本}` + 后续行 muted（错误含恢复动作文本）。
fn render_system_note_lines(data: &TuiSystemNote, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    // 剥离旧版 ✻/⏿ 前缀标记，统一走新符号层
    let clean: Vec<String> = data
        .text
        .lines()
        .map(|l| {
            l.trim_start()
                .trim_start_matches('\u{273b}')
                .trim_start_matches("\u{23bf}")
                .trim_start()
                .to_string()
        })
        .collect();

    match data.level {
        TuiNoteLevel::Info => {
            let label = clean.first().cloned().unwrap_or_default();
            vec![divider_with_label_line(grid, &label)]
        }
        TuiNoteLevel::Warning => {
            let (symbol, color) = (sym().warning, sem.status.warning);
            render_note_lines(grid, symbol, color, &clean)
        }
        TuiNoteLevel::Error => {
            let (symbol, color) = (sym().error, sem.status.error);
            render_note_lines(grid, symbol, color, &clean)
        }
    }
}

fn render_note_lines(
    grid: &GridSpec,
    symbol: &str,
    color: Color,
    clean: &[String],
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = Vec::new();
    for (i, text) in clean.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let mut spans = if i == 0 {
            first_prefix(
                grid,
                symbol,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        } else {
            cont_prefix(grid, sem.text.dim)
        };
        let fg = if i == 0 { color } else { sem.text.muted };
        spans.push(Span::styled(
            truncate_by_width(text, grid.content_width()),
            Style::default().fg(fg),
        ));
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        // 空 note 兜底：至少渲染一条符号行
        lines.push(Line::from(first_prefix(
            grid,
            symbol,
            Style::default().fg(color),
        )));
    }
    lines
}

/// `── {label} ──…` divider 行（label 为 None 时纯分隔线），填满 content 列。
fn divider_with_label_line(grid: &GridSpec, label: &str) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let inner = match label {
        "" => "\u{2500}\u{2500}".to_string(),
        l => format!("\u{2500}\u{2500} {l}"),
    };
    let inner = truncate_by_width(&inner, content);
    let fill = content.saturating_sub(inner.width());
    let mut spans = first_prefix(grid, "\u{2500}", Style::default().fg(sem.text.dim));
    spans.push(Span::styled(
        format!("{}{}", inner, "\u{2500}".repeat(fill)),
        Style::default().fg(sem.text.dim),
    ));
    Line::from(spans)
}

/// §6.7 SubAgent 单行摘要：`[◐|✓|×][gap]{Name 定宽}  {activity}  {N tools}`。
///
/// 停止递归内联铺开——嵌套消息不进入主时间轴（Enter 打开详情面板）。
/// failed 追加原因行（muted 正文，error 仅符号/accent）。
fn render_subagent_group_lines(data: &TuiSubAgentGroup, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let summary = SubAgentSummary::derive(&data.view_models, data.is_running);
    let (symbol, symbol_color) = match summary.status {
        EntryStatus::Running => (sym().running, sem.status.running),
        EntryStatus::Error => (sym().error, sem.status.error),
        EntryStatus::Completed => (sym().success, sem.status.success),
    };

    let mut spans = first_prefix(grid, symbol, Style::default().fg(symbol_color));
    // 前缀宽度稳定：名称定宽截断（§6.7 running 摘要更新不改前缀列）
    spans.push(Span::styled(
        truncate_by_width(&data.agent_name, SUBAGENT_NAME_WIDTH),
        Style::default()
            .fg(sem.text.primary)
            .add_modifier(Modifier::BOLD),
    ));
    let activity = match summary.status {
        EntryStatus::Running => summary.activity,
        _ => summary.result,
    };
    if !activity.is_empty() {
        let budget = content.saturating_sub(SUBAGENT_NAME_WIDTH + 6);
        let act = truncate_by_width(&activity, budget.max(1));
        spans.push(Span::styled(
            format!("  {act}"),
            Style::default().fg(sem.text.muted),
        ));
    }
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    // 工具计数只在有工具时显示（§6.7）——subagent 刚启动、子工具尚未路由时
    // 显示 `0 次工具` 是空壳噪音；计数从 0→N 只出现在行尾 meta 位，前缀不变。
    if summary.tool_count > 0 {
        let tool_meta = i18n::tr_args(
            "render-subagent-tools",
            &[(
                "count".to_string(),
                FluentValue::from(summary.tool_count as u64),
            )],
        );
        if let Some(meta) = place_meta(grid, used, &tool_meta, Style::default().fg(sem.text.dim)) {
            spans.extend(meta);
        }
    }
    fit_summary_to_content(&mut spans, grid);
    let mut lines = vec![Line::from(spans)];

    // failed 原因行（§6.7：failed 自动显示错误原因；正文保持可读，不整块染红）
    if summary.status == EntryStatus::Error
        && let Some(reason) = summary.last_error
        && !reason.is_empty()
    {
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&reason, content),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    lines
}

/// §6.7 从嵌套 VM 派生 SubAgent 单行摘要（确定性纯函数，测试覆盖矩阵）。
///
/// - `status`：Running（is_running）→ Error（任一 child tool error）→ Completed；
/// - `activity`（running）：反向扫描最近的非空候选（工具 input_summary 或
///   文本首行）——摘要可更新但前缀稳定；
/// - `result`（completed）：反向扫描最近的文本末行 / 工具输出 / 输入摘要；
/// - `last_error`：反向扫描第一个 error 工具的 output_summary。
pub(super) fn derive_subagent_summary(view_models: &im::Vector<TuiRenderUnit>) -> SubAgentSummary {
    let mut summary = SubAgentSummary::default();
    let mut seen_any = false;
    let mut last_error: Option<String> = None;
    for vm in view_models.iter() {
        match vm {
            TuiRenderUnit::TuiToolCard(t) => {
                seen_any = true;
                summary.tool_count += 1;
                if t.is_error {
                    summary.failed_count += 1;
                    if last_error.is_none() {
                        last_error = Some(first_line(&t.output_summary));
                    }
                }
            }
            TuiRenderUnit::TuiAssistantBubble(b) => {
                seen_any = seen_any || !b.text.is_empty();
            }
            _ => {}
        }
    }
    if !seen_any {
        return summary;
    }
    // 反向扫描最新活动/结果
    for vm in view_models.iter().rev() {
        match vm {
            TuiRenderUnit::TuiToolCard(t) => {
                if summary.activity.is_empty() && !t.input_summary.is_empty() {
                    summary.activity = first_line(&t.input_summary);
                }
                if summary.result.is_empty() {
                    if !t.output_summary.is_empty() {
                        summary.result = first_line(&t.output_summary);
                    } else if summary.activity.is_empty() && !t.input_summary.is_empty() {
                        summary.result = first_line(&t.input_summary);
                    }
                }
            }
            TuiRenderUnit::TuiAssistantBubble(b) if !b.text.is_empty() => {
                let first = first_line(&b.text);
                if summary.activity.is_empty() {
                    summary.activity = first.clone();
                }
                if summary.result.is_empty() {
                    summary.result = first;
                }
            }
            _ => {}
        }
    }
    summary.last_error = last_error.filter(|s| !s.is_empty());
    summary
}

/// SubAgent 单行摘要（§6.7）——从嵌套 VM 派生，不进入 VM/hash（hash 已含
/// child VM 的 content_hash 组合，摘要完全由 children 决定）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SubAgentSummary {
    pub status: EntryStatus,
    /// running 摘要（最近工具/文本）。
    pub activity: String,
    /// 完成结果摘要（最近文本/输出）。
    pub result: String,
    pub tool_count: usize,
    pub failed_count: usize,
    /// 首个 error 工具的输出首行。
    pub last_error: Option<String>,
}

impl SubAgentSummary {
    /// 完整推导（含 status）——供渲染与测试共用。
    pub fn derive(view_models: &im::Vector<TuiRenderUnit>, is_running: bool) -> Self {
        let mut s = derive_subagent_summary(view_models);
        s.status = if is_running {
            EntryStatus::Running
        } else if s.failed_count > 0 {
            EntryStatus::Error
        } else {
            EntryStatus::Completed
        };
        s
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// §7 折叠分组行：`[▸][gap]{title}[ · N failed]`（title 含隐藏数，
/// 如 `Read 3 · Glob 2`；failed 后缀仅组后相邻 error 数 >0 时渲染，D2）。
fn render_collapsed_group_lines(data: &TuiCollapsedGroup, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut spans = first_prefix(grid, sym().collapsed, Style::default().fg(sem.text.dim));
    // 先截 title 使 (title + failed 后缀) 总宽 ≤ content——失败数不可被截断吞掉
    // （§15「error 永不隐藏」），title 截断在前。
    let suffix = if data.failed_count > 0 {
        format!(
            " \u{b7} {}",
            i18n::tr_args(
                "render-group-failed-count",
                &[("count".to_string(), FluentValue::from(data.failed_count))],
            )
        )
    } else {
        String::new()
    };
    let keep = grid.content_width().saturating_sub(suffix.width()).max(1);
    let title_trunc = truncate_by_width(&data.title, keep);
    spans.push(Span::styled(
        title_trunc,
        Style::default().fg(sem.text.muted),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(sem.status.error)));
    }
    vec![Line::from(spans)]
}

/// 分隔线：turn 边界 divider（无 label → 纯分隔线；有 label → `── label ──`）。
fn render_divider_lines(data: &TuiDivider, grid: &GridSpec) -> Vec<Line<'static>> {
    let label = data.label.as_deref().unwrap_or("");
    vec![divider_with_label_line(grid, label)]
}

/// §6.8 Interaction block（Slice 4 双轨）：pending 态 `! Approval required` /
/// 问题摘要 / 选项行（横向 `[Allow once]  [Deny]`，Narrow 垂直排列）；
/// completed 态收束为单行结果（§7 completed → Collapsed），展开时显示
/// verb + question 与历史 items 问答对。
///
/// 选项行的「当前项」高亮（selection bg + border + bold，§9）不在本函数
/// 渲染——选项行样式依赖组件级 `use_state` 的 option_index（消息区焦点内部
/// 态），由 mod.rs 视口 post-pass 应用（与 focus/hover post-pass 同模式，
/// 不破坏按 content_hash 分片的渲染缓存，G3）。本函数只返回静态
/// `[label]` 行 + 布局信息 [`InteractionLayout`]（选项行的逻辑行与列区间）。
fn render_ask_user_block_lines(
    data: &TuiAskUserBlock,
    grid: &GridSpec,
) -> (Vec<Line<'static>>, Option<InteractionLayout>) {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let mut lines: Vec<Line<'static>> = Vec::new();

    if data.pending {
        // ── 等待响应（§6.8）：标题 + 问题摘要 + 选项行 ──
        let title = match data.kind {
            crate::kit::tui_render_unit::InteractionKind::Permission => {
                i18n::tr("render-interaction-title-permission")
            }
            crate::kit::tui_render_unit::InteractionKind::AskUser => {
                i18n::tr("render-interaction-title-ask-user")
            }
        };
        lines.push(Line::from({
            let mut spans =
                first_prefix(grid, sym().warning, Style::default().fg(sem.status.warning));
            spans.push(Span::styled(
                title,
                Style::default()
                    .fg(sem.status.warning)
                    .add_modifier(Modifier::BOLD),
            ));
            spans
        }));
        if !data.question.is_empty() {
            let mut spans = cont_prefix(grid, sem.text.dim);
            spans.push(Span::styled(
                truncate_by_width(&data.question, content),
                Style::default().fg(sem.text.muted),
            ));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));

        // 选项行：Narrow（§11）垂直排列（每行一个 `[label]`）；否则横向一行。
        let mut layout = InteractionLayout {
            option_rows: Vec::new(),
            option_cols: Vec::new(),
        };
        if grid.is_narrow() || data.options.len() <= 1 {
            for label in &data.options {
                let text = format!("[{label}]");
                let mut spans = cont_prefix(grid, sem.text.dim);
                spans.push(Span::styled(
                    truncate_by_width(&text, content),
                    Style::default().fg(sem.text.primary),
                ));
                lines.push(Line::from(spans));
                layout.option_rows.push(lines.len() - 1);
                layout.option_cols.push(None);
            }
        } else {
            // 横向：所有选项拼接在一行，空格分隔（§6.8 `[Allow once]  [Deny]`）。
            let text = data
                .options
                .iter()
                .map(|o| format!("[{o}]"))
                .collect::<Vec<_>>()
                .join("  ");
            let mut spans = cont_prefix(grid, sem.text.dim);
            spans.push(Span::styled(
                truncate_by_width(&text, content),
                Style::default().fg(sem.text.primary),
            ));
            lines.push(Line::from(spans));
            // 每选项的列区间（逻辑列 = 前缀宽 + 前序选项累计宽；超宽截断的行
            // 不生成点击区间——点击事件按列命中，超宽部分不可点）。
            let prefix_w = grid.cont_prefix_width();
            let mut col = prefix_w;
            for label in &data.options {
                let w = format!("[{label}]").width();
                if col + w <= prefix_w + content {
                    layout
                        .option_cols
                        .push(Some((col as u16, (col + w) as u16)));
                } else {
                    layout.option_cols.push(None);
                }
                col += w + 2;
            }
            layout.option_rows.push(lines.len() - 1);
        }
        return (lines, Some(layout));
    }

    // ── 已答复（§7 completed → Collapsed 结果行；展开显示 verb + question）──
    let result = data.result.as_deref().unwrap_or("");
    let (symbol, color) = if data.is_error {
        (sym().error, sem.status.error)
    } else {
        (sym().success, sem.status.success)
    };
    let title = if !result.is_empty() {
        result.to_string()
    } else if !data.question.is_empty() {
        data.question.clone()
    } else {
        i18n::tr("render-user-answered")
    };
    lines.push(Line::from({
        let mut spans = first_prefix(grid, symbol, Style::default().fg(color));
        spans.push(Span::styled(
            truncate_by_width(&title, content),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans
    }));
    // Collapsed（§7 completed 默认）→ 单行结果；展开时附加摘要与历史问答对。
    if data.fold == FoldState::Collapsed {
        return (lines, None);
    }
    // 展开时附加 verb + question 行（用户手动展开后可见完整摘要）。
    if !data.question.is_empty() {
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&data.question, content),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    // 历史 items 问答对（旧数据兼容；生产路径 items 恒空）。
    for item in &data.items {
        let text = format!("{} \u{2192} {}", item.header, item.answer);
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&text, content),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    (lines, None)
}

/// Interaction block 选项行的布局信息（供 mod.rs 视口 post-pass 应用
/// 「当前项」高亮与点击热区——与 `CopyButtonInfo` 同模式）。
#[derive(Debug, Clone, Default)]
pub(crate) struct InteractionLayout {
    /// 每个 option 所在 slot lines 的逻辑行索引。
    pub option_rows: Vec<usize>,
    /// 每 option 在行内的列区间（逻辑列；垂直/超宽时为 None = 整行命中）。
    pub option_cols: Vec<Option<(u16, u16)>>,
}

/// §6.9 Todo 进度摘要行：`[◼][gap]{3/7 tasks · Running tests}`。
fn render_todo_summary_lines(data: &TuiTodoSummary, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut spans = first_prefix(grid, "\u{25fc}", Style::default().fg(sem.accent));
    spans.push(Span::styled(
        truncate_by_width(&data.text, grid.content_width()),
        Style::default().fg(sem.text.muted),
    ));
    vec![Line::from(spans)]
}

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;
