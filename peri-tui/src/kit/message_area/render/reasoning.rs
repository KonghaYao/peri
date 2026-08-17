use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{FoldState, TuiReasoningBlock};
use crate::truncate::wrap_by_width;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{cont_prefix, first_prefix, place_meta};

/// Expanded reasoning 正文的最大行数（防超长推理块撑爆视口）。
const REASONING_BODY_MAX_LINES: usize = 100;

/// §6.3 Reasoning 三态视觉。
///
/// - Running/Preview：`[│][gap]Thinking…`（bold, status.running）+ elapsed（三档放置）
///   + 最近 ≤4 个视觉行 tail（wrap 后取尾，长行显示尾部而非截断头部；muted+italic，
///     无 italic → dim；`│` 续行前缀与 md 正文一致）。
/// - Completed/Collapsed：单行 `[│][gap]Thought for 12s · N lines`（N = 视觉行总数）。
/// - Completed/Expanded：`[│][gap]Thought for 12s · N lines` + 前 ≤100 个视觉行正文。
/// - 空 reasoning 仍显示 `Thinking…`（不出现空白 block）。
/// - 行数口径统一为视觉行：`reasoning_visual_lines` 一次 wrap 出视觉行序列，tail /
///   摘要 N / Expanded 正文共用——渲染行数与滚动高度（build_wrap_map）天然一致。
/// - [用户需求] 首行 icon 三态统一为竖线 `│`（与正文续行前缀同形，视觉连续）；
///   状态感由颜色承担（Running=status.running / Completed=dim）。
///
/// §6.3 视觉行单一事实源：先按 `\n` 分行、逐行 wrap 成视觉行，再丢弃 trim 空行。
/// tail / 摘要 N / Expanded 正文共用此序列——渲染行数与滚动高度天然一致。
fn reasoning_visual_lines(text: &str, width: usize) -> Vec<String> {
    text.lines()
        .flat_map(|l| wrap_by_width(l, width))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

pub(super) fn render_reasoning_block(
    reasoning: &TuiReasoningBlock,
    grid: &GridSpec,
) -> Vec<Line<'static>> {
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
    // 一次 wrap 出视觉行序列（tail / 摘要 N / Expanded 正文共用，§6.3 单一事实源）。
    let visual_lines = reasoning_visual_lines(&reasoning.text, grid.content_width());
    let line_count = visual_lines.len();

    if reasoning.is_running {
        // Running（fold=Preview 或用户覆盖 Expanded）：│ Thinking… + elapsed + tail。
        // [Fix §6.3] 用户手动折叠（Collapsed）→ 仅活动状态行，不渲染 tail——
        // 「隐藏 reasoning 只影响 body；活动状态行仍需可见」（§7 running 默认
        // Preview，Collapsed 只能来自用户覆盖——Space 切换必须有视觉反馈）。
        // 首行 icon 统一竖线（用户需求）；活动感由 running 色 + elapsed + tail
        // 增长承担（§8.2 动画帧不再用于 reasoning）。
        let mut spans = first_prefix(grid, "\u{2502}", Style::default().fg(sem.status.running));
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
        for tail in visual_lines.iter().rev().take(tail_max).rev() {
            let mut spans = cont_prefix(grid, sem.accents.reasoning);
            spans.push(Span::styled(tail.clone(), body_style));
            lines.push(Line::from(spans));
        }
    } else {
        // Completed：折叠单行 / 展开含正文。
        // 首行 icon 统一竖线（用户需求）——折叠/展开差异由正文是否渲染承担，
        // 不再用 ▸/▾ 箭头区分（与 Running 同形，视觉连续）。
        // [Fix LOW-6] Preview（§7 表 completed 只定义 Collapsed；Space 可从
        // Collapsed 切到 Preview）映射到单行折叠视觉——无正文即单行，无
        // 「假展开」误导（正文只在 Expanded 渲染）。
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
        let mut spans = first_prefix(grid, "\u{2502}", Style::default().fg(sem.text.dim));
        spans.push(Span::styled(summary, Style::default().fg(sem.text.dim)));
        lines.push(Line::from(spans));

        if reasoning.fold == FoldState::Expanded {
            for body_line in visual_lines.iter().take(REASONING_BODY_MAX_LINES) {
                let mut spans = cont_prefix(grid, sem.accents.reasoning);
                spans.push(Span::styled(body_line.clone(), body_style));
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}
