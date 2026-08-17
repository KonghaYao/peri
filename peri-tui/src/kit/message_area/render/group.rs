use crate::i18n;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{
    EntryStatus, TuiCollapsedGroup, TuiRenderUnit, TuiSubAgentGroup, TuiToolCard,
};
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{
    cont_prefix, first_prefix, fit_summary_to_content, format_completed_duration,
    format_running_duration, place_meta, status_symbol_and_color, sym,
};

/// running 子 agent 最多展示的最近工具调用行数（§6.7 工具行展示）。
pub(super) const SUBAGENT_TOOL_LINES: usize = 3;
/// 子 agent 工具行相对 content 列的固定缩进（设计文档 §3 ②——固定 2 格而非
/// 对齐 subagent 摘要：流式最近 3 行持续轮换，固定缩进保证前缀列稳定）。
pub(super) const SUBAGENT_TOOL_INDENT: usize = 2;

/// §6.7 SubAgent：组只展示子工具调用（任意状态 running/completed/error 同款
/// 工具行样式：最近 ≤3 个工具行 + 失败原因行）；组内无任何工具调用时不渲染
/// 组头，整组留空（仅 genuine parent error 保留原因行）。
///
/// 停止递归内联铺开——嵌套消息不进入主时间轴（Enter 打开详情面板）。
pub(super) fn render_subagent_group_lines(
    data: &TuiSubAgentGroup,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
    // parent 终态由 canonical is_error 决定（SubagentStopped.is_error）；
    // nested child tool error 保持局部可见但不提升 block error。
    let summary = SubAgentSummary::derive(&data.view_models, data.is_running, data.is_error);

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
    if recent.is_empty() {
        // 组内无任何工具调用：不渲染组头——仅 genuine parent error 保留原因行
        // （错误信息不丢），其余整组留空（子 agent 纯文本/空跑不占消息区空间）。
        if data.is_error {
            let reason = data
                .error_reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .or_else(|| summary.last_error.as_deref().filter(|r| !r.is_empty()));
            if let Some(reason) = reason {
                return vec![subagent_error_reason_line(grid, reason)];
            }
        }
        return Vec::new();
    }
    let mut lines: Vec<Line<'static>> =
        recent.iter().map(|t| subagent_tool_line(t, grid)).collect();
    // 失败原因行（§6.7 failed 显示错误原因），优先级：canonical error_reason
    // （SubagentStopped.result）→ 子工具 last_error 兜底。仅 running（实时
    // 反馈）与 genuine parent error（终态）显示；completed 成功组即使有
    // nested child tool error 也不显示（保持原 §6.7 语义）。
    let reason = if data.is_error {
        data.error_reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .or_else(|| summary.last_error.as_deref().filter(|r| !r.is_empty()))
    } else if data.is_running {
        summary.last_error.as_deref().filter(|r| !r.is_empty())
    } else {
        None
    };
    if let Some(reason) = reason {
        lines.push(subagent_error_reason_line(grid, reason));
    }
    lines
}

/// §6.7 单个子工具调用行（设计文档 §3 形态规格）：
/// `[cont_prefix][2 格缩进][⠋|✓|×] {Verb}  {summary}  {duration}`——嵌套从属
/// 弱化形态：续行竖线 + 固定缩进 + 无 bold label + dim 符号，与主时间轴 tool
/// activity row（bold primary + 状态色符号）差异化（P2）；error 符号与错误词
/// 不弱化（P3 错误不弱化）。全部行（含首行）走 cont_prefix 风格——工具行永远
/// 不是独立 entry 首行，结构上即从属。
fn subagent_tool_line(card: &TuiToolCard, grid: &GridSpec) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let content = grid.content_width();
    let mut spans = cont_prefix(grid, sem.text.dim);
    spans.push(Span::raw(" ".repeat(SUBAGENT_TOOL_INDENT)));
    // Narrow 断点：符号位省略（设计文档 §6 断点表——极端窄屏接受状态字符
    // 丢失，错误信号由错误词与原因行兜底；§11 与主时间线 accent 退化同哲学）。
    if !grid.is_narrow() {
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
    // duration：running 秒 / completed 冻结值（与工具卡片同一口径）。
    let duration_text = if card.is_running {
        card.running_duration_ms.map(format_running_duration)
    } else {
        card.completed_duration_ms.map(format_completed_duration)
    };
    // error 错误词 ` — Failed`（§6.4 主时间线同款，P3 错误不弱化）。
    let error_word = if card.is_error {
        format!(" \u{2014} {}", i18n::tr("msg-status-failed"))
    } else {
        String::new()
    };
    // summary 预算：预留缩进、label、错误词与 duration 列，避免截断后无处可放。
    let dur_w = duration_text.as_deref().map(|d| d.width()).unwrap_or(0);
    let budget = content
        .saturating_sub(SUBAGENT_TOOL_INDENT + name.width() + error_word.width() + dur_w + 6)
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
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    if let Some(meta) = duration_text
        .as_deref()
        .and_then(|d| place_meta(grid, used, d, Style::default().fg(sem.text.dim)))
    {
        spans.extend(meta);
    }
    fit_summary_to_content(&mut spans, grid);
    Line::from(spans)
}

/// 子 agent 失败原因行（§6.7：failed 自动显示错误原因；正文保持可读，不整块染红）。
/// 前缀与工具行同列对齐（cont_prefix + 固定 2 格缩进，设计文档 §5）——与工具行
/// 同属一层级；正文预算减去缩进宽度（保持整行 ≤ content 列，不越右缘）。
fn subagent_error_reason_line(grid: &GridSpec, reason: &str) -> Line<'static> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut spans = cont_prefix(grid, sem.text.dim);
    spans.push(Span::raw(" ".repeat(SUBAGENT_TOOL_INDENT)));
    spans.push(Span::styled(
        truncate_by_width(
            reason,
            grid.content_width().saturating_sub(SUBAGENT_TOOL_INDENT),
        ),
        Style::default().fg(sem.text.muted),
    ));
    Line::from(spans)
}

/// §6.7 从嵌套 VM 派生 SubAgent 摘要（确定性纯函数，测试覆盖矩阵）。
///
/// - `status`：Running（is_running）→ Error（canonical `is_error`，来自
///   `SubagentStopped.is_error`）→ Completed；nested child tool error 只计入
///   `failed_count`/`last_error`，不决定 parent status（见 [`SubAgentSummary::derive`]）；
/// - `last_error`：第一个 error 工具的 output_summary 首行。
///
/// 注：`activity`/`result`（单行组头摘要）已随组头渲染取消而移除——组只展示
/// 嵌套工具行，渲染层不再消费文本摘要。
pub(super) fn derive_subagent_summary(view_models: &im::Vector<TuiRenderUnit>) -> SubAgentSummary {
    let mut summary = SubAgentSummary::default();
    let mut last_error: Option<String> = None;
    for vm in view_models.iter() {
        if let TuiRenderUnit::TuiToolCard(t) = vm {
            summary.tool_count += 1;
            if t.is_error {
                summary.failed_count += 1;
                if last_error.is_none() {
                    last_error = Some(first_line(&t.output_summary));
                }
            }
        }
    }
    summary.last_error = last_error.filter(|s| !s.is_empty());
    summary
}

/// SubAgent 摘要（§6.7）——从嵌套 VM 派生，不进入 VM/hash（hash 已含
/// child VM 的 content_hash 组合，摘要完全由 children 决定）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SubAgentSummary {
    pub status: EntryStatus,
    pub tool_count: usize,
    pub failed_count: usize,
    /// 首个 error 工具的输出首行。
    pub last_error: Option<String>,
}

impl SubAgentSummary {
    /// 完整推导（含 status）——供渲染与测试共用。
    /// `is_error` 为 parent 终态唯一事实源（`SubagentStopped.is_error`）。
    pub fn derive(
        view_models: &im::Vector<TuiRenderUnit>,
        is_running: bool,
        is_error: bool,
    ) -> Self {
        let mut s = derive_subagent_summary(view_models);
        s.status = if is_running {
            EntryStatus::Running
        } else if is_error {
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
pub(super) fn render_collapsed_group_lines(
    data: &TuiCollapsedGroup,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
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
