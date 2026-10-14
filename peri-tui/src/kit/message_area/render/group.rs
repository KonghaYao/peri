use crate::i18n;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{
    FoldState, TuiCollapsedGroup, TuiRenderUnit, TuiSubAgentGroup, TuiToolCard,
};
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{
    fit_summary_to_content, format_completed_duration, format_running_duration, place_meta,
    process_cont_prefix, process_first_prefix, status_symbol_and_color, sym,
};

/// running 子 agent 最多展示的最近工具调用行数（§6.7 工具行展示）。
pub(super) const SUBAGENT_TOOL_LINES: usize = 3;

/// §6.7 SubAgent：有工具时展示最近 ≤3 个工具行 + 失败原因行；无工具时
/// 展示一行可聚焦的 Agent 入口，供纯文本子会话打开详情。
///
/// 停止递归内联铺开——嵌套消息不进入主时间轴（Enter 打开详情面板）。
///
/// 过程区缩进只由过程区前缀承担：组头与组内工具行、失败原因行同列，不再叠加
/// 子工具级缩进；文本区 → 过程区的入口台阶由 render 层在转场后的 entry 行首
/// 插入（§6），组内渲染器不感知转场。
pub(super) fn render_subagent_group_lines(
    data: &TuiSubAgentGroup,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    // parent 终态由 canonical is_error 决定（SubagentStopped.is_error）；
    // nested child tool error 保持局部可见但不提升 block error。
    // 组只展示子工具调用（任意状态同款样式，§6.7 工具行展示）：
    // 最近几个工具行 + 失败原因行。
    let recent: Vec<&TuiToolCard> = data
        .view_models
        .iter()
        .rev()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) => Some(t),
            _ => None,
        })
        .take(SUBAGENT_TOOL_LINES)
        .collect();
    let mut lines: Vec<Line<'static>> = if recent.is_empty() {
        // 无工具时组头行即 entry 首行。
        vec![subagent_entry_line(data, grid)]
    } else {
        // 工具行结构上是过程区续行（永远不是 entry 首行，设计文档 §3）——统一走
        // 续行前缀，组头/工具行/原因行同列；入口台阶因此不落在工具行上。
        recent.iter().map(|t| subagent_tool_line(t, grid)).collect()
    };
    // 失败原因行（§6.7 failed 显示错误原因），优先级：canonical error_reason
    // （SubagentStopped.result）→ 子工具 last_error 兜底。仅 running（实时
    // 反馈）与 genuine parent error（终态）显示；completed 成功组即使有
    // nested child tool error 也不显示（保持原 §6.7 语义）。
    if let Some(reason) = subagent_error_reason(data) {
        lines.extend(subagent_error_reason_lines(grid, reason));
    }
    lines
}

fn subagent_entry_line(data: &TuiSubAgentGroup, grid: &GridSpec) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let (symbol, color) = status_symbol_and_color(data.is_running, data.is_error, &sem);
    let mut spans = process_first_prefix(grid, &symbol, Style::default().fg(color));
    let name = if data.agent_name.trim().is_empty() {
        &data.agent_id
    } else {
        &data.agent_name
    };
    spans.push(Span::styled(
        truncate_by_width(name, grid.process_content_width().max(1)),
        Style::default()
            .fg(sem.text.primary)
            .add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

/// §6.7 单个子工具调用行（设计文档 §3 形态规格）：
/// `[outer 空][indent 留空][│][gap]{⠋|✓|×} {Verb}  {summary}  {duration}`——嵌套
/// 从属弱化形态：过程区续行前缀 + 无 bold label + dim 符号，与主时间线 tool
/// activity row（bold primary + 状态色符号）差异化（P2）；error 符号与错误词
/// 不弱化（P3 错误不弱化）。全部行（含首行）走过程区续行风格——工具行永远
/// 不是独立 entry 首行，结构上即从属；缩进由过程区前缀承担（与组头同列）。
fn subagent_tool_line(card: &TuiToolCard, grid: &GridSpec) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.process_content_width();
    let mut spans = process_cont_prefix(grid, sem.text.dim);
    // Narrow 断点：符号位省略（设计文档 §6 断点表——极端窄屏接受状态字符
    // 丢失，错误信号由错误词与原因行兜底；§11 与主时间线 accent 退化同哲学）。
    if !grid.is_narrow() || card.is_error {
        // 符号复用状态三元组（running braille 帧 / ✓ / ×）；颜色就地决定：
        // running/success 低显著 text.dim（P2），error 升级 status.error（P3）。
        let (symbol, _) = status_symbol_and_color(card.is_running, card.is_error, &sem);
        let symbol_color = if card.is_error {
            sem.status.error
        } else {
            sem.text.dim
        };
        spans.push(Span::styled(
            format!("{symbol} "),
            Style::default().fg(symbol_color),
        ));
    }
    // label 去 bold（P2 权重弱化——bold + 状态色是主时间线工具的专属锚点）。
    let name = crate::kit::tool_display::format_tool_name(&card.tool_name);
    spans.push(Span::styled(
        name.clone(),
        Style::default().fg(sem.text.primary),
    ));
    // duration：running 秒 / completed 冻结值（与工具卡片同一口径；亚秒极快
    // 无值可显示时不占位）。
    let duration_text = if card.is_running {
        card.running_duration_ms.map(format_running_duration)
    } else {
        card.completed_duration_ms
            .and_then(format_completed_duration)
    };
    // error 错误词 ` — Failed`（§6.4 主时间线同款，P3 错误不弱化）。
    let error_word = if card.is_error {
        format!(" \u{2014} {}", i18n::tr("msg-status-failed"))
    } else {
        String::new()
    };
    // summary 预算：预留符号位、label、错误词与 duration 列，避免截断后无处可放。
    let dur_w = duration_text.as_deref().map(|d| d.width()).unwrap_or(0);
    let budget = content
        .saturating_sub(name.width() + error_word.width() + dur_w + 6)
        .max(1);
    if !card.input_summary.is_empty() {
        let summary = truncate_by_width(&card.input_summary, budget);
        if !summary.is_empty() {
            spans.push(Span::styled(
                format!("  {summary}"),
                Style::default().fg(sem.text.muted),
            ));
        }
    }
    // 错误词：summary 之后、duration 之前（§6.4 同序），参与 used 计算。
    if !error_word.is_empty() {
        spans.push(Span::styled(
            error_word,
            Style::default()
                .fg(sem.status.error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // 正文裁剪先于 gutter 放置（helpers `fit_summary_to_content` 的调用约定）：
    // duration 是右缘元数据，不做正文裁剪的目标；`used` 在裁剪后重算。
    fit_summary_to_content(&mut spans, grid);
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    if let Some(meta) = duration_text
        .as_deref()
        .and_then(|d| place_meta(grid, used, d, Style::default().fg(sem.text.dim)))
    {
        spans.extend(meta);
    }
    Line::from(spans)
}

/// 子 agent 失败原因行（§6.7：failed 自动显示错误原因；正文保持可读，不整块染红）。
/// 前缀与工具行同列（过程区续行前缀，缩进由过程区承担、不叠加子工具缩进）——与
/// 工具行同属一层级；正文预算减去符号位宽度（保持整行不越过程区预算，§8）。
fn subagent_error_reason_lines(grid: &GridSpec, reason: &str) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    super::error::preview_lines(reason, grid.process_content_width().saturating_sub(2), 3)
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let mut spans = process_cont_prefix(grid, sem.text.dim);
            spans.push(Span::styled(
                if index == 0 {
                    format!("{} ", sym().error)
                } else {
                    "  ".to_owned()
                },
                Style::default().fg(sem.status.error),
            ));
            spans.push(Span::styled(text, Style::default().fg(sem.text.muted)));
            Line::from(spans)
        })
        .collect()
}

pub(super) fn subagent_error_reason(data: &TuiSubAgentGroup) -> Option<&str> {
    let fallback = || {
        data.view_models.iter().rev().find_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card)
                if card.is_error && !card.output_summary.is_empty() =>
            {
                Some(card.output_summary.as_str())
            }
            _ => None,
        })
    };
    if data.is_error {
        data.error_reason
            .as_deref()
            .filter(|reason| !reason.is_empty())
            .or_else(fallback)
    } else if data.is_running {
        fallback()
    } else {
        None
    }
}

/// §7 折叠分组行：`[outer 空][indent 留空][▸|▾][gap]{title}[ · N failed]`（title
/// 含隐藏数，如 `Read 3 · Glob 2`；failed 后缀仅组后相邻 error 数 >0 时渲染，D2）。
///
/// 展开时组内成员按过程区接续渲染（`vm_to_lines_cached` 固定
/// [`ZoneTransition::None`](crate::kit::message_area::grid::ZoneTransition::None)）
/// ——同区继续，不重复画入口台阶；成员与组头同列。
pub(super) fn render_collapsed_group_lines(
    data: &TuiCollapsedGroup,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let expanded = data.fold == FoldState::Expanded;
    let symbol = if expanded {
        sym().expanded
    } else {
        sym().collapsed
    };
    let mut spans = process_first_prefix(grid, symbol, Style::default().fg(sem.text.dim));
    // 先截 title 使 (title + failed 后缀) 总宽 ≤ 过程区预算——失败数不可被截断吞掉
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
    let content_width = grid.process_content_width();
    let keep = content_width.saturating_sub(suffix.width()).max(1);
    let title_trunc = truncate_by_width(&data.title, keep);
    spans.push(Span::styled(
        title_trunc,
        Style::default().fg(sem.text.muted),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(sem.status.error)));
    }
    let mut lines = vec![Line::from(spans)];
    if expanded {
        let mut md_cache = crate::kit::markdown::MarkdownRenderCache::default();
        for vm in &data.view_models {
            lines.extend(super::vm_to_lines_cached(vm, grid, &mut md_cache, false).0);
        }
    }
    lines
}
