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
    let plan = project_tool_render_plan(data, grid);
    let resolved = resolve_tool_render_plan(data, plan);
    render_tool_plan(data, resolved, grid)
}

/// Presentation 只投影 header/detail 候选内容；状态、fold、prefix 与时长由
/// `resolve_tool_render_plan` / `render_tool_plan` 的统一路径消费。
struct ToolRenderPlan {
    label: String,
    summary: String,
    completed_suffix: String,
    hide_completed_summary_when_expanded: bool,
    running_details: Vec<Line<'static>>,
    completed_details: Vec<Line<'static>>,
    error_details: Vec<Line<'static>>,
}

struct ResolvedToolRenderPlan {
    label: String,
    summary: String,
    suffix: String,
    details: Vec<Line<'static>>,
}

fn project_tool_render_plan(data: &TuiToolCard, grid: &GridSpec) -> ToolRenderPlan {
    match &data.presentation {
        TuiToolPresentation::Generic => project_generic_tool(data, grid),
        TuiToolPresentation::Skill(skill) => project_skill_tool(&skill.name),
        TuiToolPresentation::Todo(todo) => project_todo_tool(data, todo, grid),
    }
}

/// 语义复制与视觉 header 共用同一投影，避免 presentation 文案双重实现。
pub(super) fn projected_tool_header_text(data: &TuiToolCard, grid: &GridSpec) -> String {
    let plan = resolve_tool_render_plan(data, project_tool_render_plan(data, grid));
    let mut text = plan.label;
    if !plan.summary.is_empty() {
        text.push(' ');
        text.push_str(&plan.summary);
    }
    text.push_str(&plan.suffix);
    text
}

/// 生命周期只在此处解析一次：presentation 提供候选内容，统一路径选择当前状态
/// 对应的 suffix/detail，并处理展开态 header 摘要。
fn resolve_tool_render_plan(data: &TuiToolCard, plan: ToolRenderPlan) -> ResolvedToolRenderPlan {
    let ToolRenderPlan {
        label,
        mut summary,
        completed_suffix,
        hide_completed_summary_when_expanded,
        running_details,
        completed_details,
        error_details,
    } = plan;
    if hide_completed_summary_when_expanded
        && !data.is_running
        && !data.is_error
        && data.fold == FoldState::Expanded
    {
        summary.clear();
    }
    let (suffix, details) = if data.is_running {
        (String::new(), running_details)
    } else if data.is_error {
        (String::new(), error_details)
    } else {
        (completed_suffix, completed_details)
    };
    ResolvedToolRenderPlan {
        label,
        summary,
        suffix,
        details,
    }
}

/// 工具卡片的唯一结构 renderer。所有 presentation 共用同一条状态与折叠路径：
/// header 恒可见；Collapsed 不消费 detail；Preview/Expanded 消费解析后的 detail。
fn render_tool_plan(
    data: &TuiToolCard,
    plan: ResolvedToolRenderPlan,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let (status_symbol, symbol_color) =
        status_symbol_and_color(data.is_running, data.is_error, &sem);
    let symbol = if !data.is_running && !data.is_error && data.fold == FoldState::Expanded {
        sym().expanded.to_string()
    } else {
        status_symbol
    };
    let mut spans = first_prefix(grid, &symbol, Style::default().fg(symbol_color));
    let label = truncate_by_width(&plan.label, grid.content_width().max(1));
    let label_width = label.width();
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(sem.text.primary)
            .add_modifier(Modifier::BOLD),
    ));

    let error_word = if data.is_error && grid.content_width() > label_width.saturating_add(2) {
        format!(" \u{2014} {}", i18n::tr("msg-status-failed"))
    } else {
        String::new()
    };
    let fixed_width = error_word.width() + plan.suffix.width() + 2;
    let used_width: usize = spans.iter().map(|span| span.content.width()).sum();
    let budget = grid
        .content_width()
        .saturating_sub(used_width + fixed_width)
        .max(1);
    let summary = truncate_by_width(&plan.summary, budget);
    if !summary.is_empty() {
        spans.push(Span::styled(
            format!(" {summary}"),
            Style::default().fg(sem.text.muted),
        ));
    }
    if !plan.suffix.is_empty() {
        spans.push(Span::styled(plan.suffix, Style::default().fg(sem.text.dim)));
    }
    if !error_word.is_empty() {
        spans.push(Span::styled(
            error_word,
            Style::default()
                .fg(sem.status.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let duration_text = if data.is_running {
        data.running_duration_ms.map(format_running_duration)
    } else {
        data.completed_duration_ms.map(format_completed_duration)
    };
    let used: usize = spans.iter().map(|span| span.content.width()).sum();
    if let Some(meta) = duration_text
        .as_deref()
        .and_then(|duration| place_meta(grid, used, duration, Style::default().fg(sem.text.dim)))
    {
        spans.extend(meta);
    }
    fit_summary_to_content(&mut spans, grid);

    let mut lines = vec![Line::from(spans)];
    if data.fold != FoldState::Collapsed {
        for detail in plan.details {
            let mut spans = cont_prefix(grid, sem.accents.tool);
            spans.extend(detail.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn project_skill_tool(name: &str) -> ToolRenderPlan {
    let running_details = vec![Line::from(Span::styled(
        i18n::tr("msg-status-loading"),
        Style::default().fg(THEME_ATOM.state().read().semantic.text.muted),
    ))];
    ToolRenderPlan {
        label: format!("Skill ({name})"),
        summary: String::new(),
        completed_suffix: String::new(),
        hide_completed_summary_when_expanded: false,
        running_details,
        completed_details: Vec::new(),
        error_details: Vec::new(),
    }
}

fn project_todo_tool(
    data: &TuiToolCard,
    todo: &TuiTodoPresentation,
    grid: &GridSpec,
) -> ToolRenderPlan {
    let sem = THEME_ATOM.state().read().semantic;
    let content_width = grid.content_width().saturating_sub(1).max(1);
    let label = format!("TodoUpdate ({}/{})", todo.completed_count, todo.total_count);
    let completed_details: Vec<Line<'static>> = todo
        .changes
        .iter()
        .map(|change| {
            let symbols = sym();
            let (icon, color) = match change.kind {
                TuiTodoChangeKind::Completed => (symbols.success, sem.status.success),
                TuiTodoChangeKind::Added => ("+", sem.status.success),
                TuiTodoChangeKind::Removed => ("-", sem.text.muted),
                TuiTodoChangeKind::Started => (symbols.todo_started, sem.status.success),
                TuiTodoChangeKind::Reopened => (symbols.todo_reopened, sem.status.success),
                TuiTodoChangeKind::ActiveFormUpdated => (symbols.todo_edited, sem.status.success),
            };
            Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    truncate_by_width(&change.content, content_width),
                    Style::default().fg(sem.text.muted),
                ),
            ])
        })
        .collect();
    let error_details = vec![Line::from(Span::styled(
        truncate_by_width(&data.output_summary, content_width),
        Style::default().fg(sem.text.muted),
    ))];
    ToolRenderPlan {
        label,
        summary: String::new(),
        completed_suffix: String::new(),
        hide_completed_summary_when_expanded: false,
        running_details: completed_details.clone(),
        completed_details,
        error_details,
    }
}

fn project_generic_tool(data: &TuiToolCard, grid: &GridSpec) -> ToolRenderPlan {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let bash = data.tool_name == "Bash";
    let mut completed_details = Vec::new();
    if bash {
        completed_details.push(Line::from(Span::styled(
            format!(
                "$ {}",
                truncate_by_width(&data.input_summary, content.saturating_sub(2))
            ),
            Style::default().fg(sem.syntax.command),
        )));
        completed_details.push(divider_fill_line(grid));
    }

    let max_lines = if matches!(grid.bp, Breakpoint::Compact | Breakpoint::Narrow) {
        2
    } else {
        TOOL_OUTPUT_MAX_LINES
    };
    let output_details = |output: &str, color| {
        compact_output_lines(output, max_lines, content)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(color))))
            .collect::<Vec<_>>()
    };
    if let Some(diff) = &data.diff {
        completed_details.extend(render_diff_lines(diff, grid));
    } else if !data.output_summary.is_empty() {
        completed_details.extend(output_details(&data.output_summary, sem.text.muted));
    }

    let error_details = if let Some(diff) = &data.diff {
        render_diff_lines(diff, grid)
    } else {
        let output = data.output_summary.replacen(" - Error: ", "\n- Error: ", 1);
        output_details(&output, sem.status.error)
    };

    ToolRenderPlan {
        label: crate::kit::tool_display::format_tool_name(&data.tool_name),
        summary: data.input_summary.clone(),
        completed_suffix: completed_header_suffix(data),
        hide_completed_summary_when_expanded: bash,
        running_details: Vec::new(),
        completed_details,
        error_details,
    }
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
    let mut spans = Vec::new();
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
    let mut hunk_header = Vec::new();
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
        let mut spans = Vec::new();
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
        let mut more = Vec::new();
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
    if data.output_summary.is_empty() {
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

/// 内容列铺满的 dim 分隔线；统一 renderer 负责添加续行前缀。
fn divider_fill_line(grid: &GridSpec) -> Line<'static> {
    Line::from(Span::styled(
        "\u{2500}".repeat(grid.content_width()),
        Style::default().fg(THEME_ATOM.state().read().semantic.text.dim),
    ))
}
