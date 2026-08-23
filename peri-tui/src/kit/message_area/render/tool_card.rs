use crate::i18n;
use crate::kit::message_area::grid::{Breakpoint, GridSpec};
use crate::kit::tui_render_unit::{
    FoldState, TuiDiffBlock, TuiHunkLineKind, TuiTodoChangeKind, TuiTodoPresentation, TuiToolCard,
    TuiToolPresentation,
};
use crate::truncate::truncate_by_width;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{
    compact_output_lines, cont_prefix, diff_change_summary, first_prefix, fit_summary_to_content,
    format_completed_duration, format_running_duration, place_meta, status_symbol_and_color, sym,
};

/// 工具输出展开的最大行数（与历史 expanded 行为一致）。
const TOOL_OUTPUT_MAX_LINES: usize = 4;

/// §6.4 Tool activity：compact 行，不是重型 card。
///
/// - 首行 `[◐|✓|×][gap]{Verb}`（单 bold 锚点）`{summary}` + 后缀 + duration 三档。
/// - summary 着色：Bash → syntax.command；Read/Write/Edit → syntax.path；其余 muted。
/// - error：× + status.error 符号色 + 明确错误词（`— Failed`），正文不整块染红。
/// - running（Preview）：仅活动行（运行中无 output）；completed：折叠单行 /
///   展开输出（≤4 行）；Bash 展开显示 `$ command` + 分隔线 + 输出。
pub(super) fn render_tool_card_lines(data: &TuiToolCard, grid: &GridSpec) -> Vec<Line<'static>> {
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
            &symbol,
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
        &symbol,
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
    with_prefix_lines(grid, &symbol, Style::default().fg(indicator_color), lines)
}

fn render_generic_tool_card_lines(data: &TuiToolCard, grid: &GridSpec) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let display_name = crate::kit::tool_display::format_tool_name(&data.tool_name);
    let (symbol, symbol_color) = status_symbol_and_color(data.is_running, data.is_error, &sem);
    // 展开态（completed+Expanded / error=Expanded）首行符号换 ▾（§6.4 示例）。
    let symbol = if !data.is_running && !data.is_error && data.fold == FoldState::Expanded {
        sym().expanded.to_string()
    } else {
        symbol
    };

    let mut spans = first_prefix(grid, &symbol, Style::default().fg(symbol_color));
    // label：每行最多一个 bold 主锚点
    spans.push(Span::styled(
        display_name.clone(),
        Style::default()
            .fg(sem.text.primary)
            .add_modifier(Modifier::BOLD),
    ));

    // summary 统一 muted 暗色——路径/命令不再用高饱和 syntax 色
    // （label 是每行唯一的亮色主锚点，summary 不与其抢视觉，§6.4）。
    let summary_color = sem.text.muted;
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
pub(super) fn completed_header_suffix(data: &TuiToolCard) -> String {
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
            let truncated = data.output_summary.contains("[Output truncated:");
            if truncated {
                format!(" \u{2014} {} lines · truncated", total_lines)
            } else {
                format!(" \u{2014} {} lines", total_lines)
            }
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
