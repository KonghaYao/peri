use crate::i18n;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{FoldState, TuiAskUserBlock};
use crate::truncate::truncate_by_width;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::helpers::{cont_prefix, first_prefix, sym};

/// §6.8 Interaction block（Slice 4 双轨）：pending 态 `! Approval required` /
/// 问题摘要 / 选项行（横向 `[Allow once]  [Deny]`，Narrow 垂直排列）；
/// completed 态完整展示（用户需求——不再自动收束）：结果行 + 问题 + 选项
/// （选中项 ✓ 标记），仅用户手动折叠（Space → Collapsed）时收束为单行结果。
///
/// 选项行的「当前项」高亮（selection bg + border + bold，§9）不在本函数
/// 渲染——选项行样式依赖组件级 `use_state` 的 option_index（消息区焦点内部
/// 态），由 mod.rs 视口 post-pass 应用（与 focus/hover post-pass 同模式，
/// 不破坏按 content_hash 分片的渲染缓存，G3）。本函数只返回静态
/// `[label]` 行 + 布局信息 [`InteractionLayout`]（选项行的逻辑行与列区间）。
pub(super) fn render_ask_user_block_lines(
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

    // ── 已答复（§6.8 完整展示：结果行 + 问题 + 选项（选中 ✓）+ 历史 items；
    // 仅用户手动折叠（Collapsed）收束为单行结果）──
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
    // Collapsed（仅来自用户手动 Space 折叠）→ 单行结果；默认 Expanded
    // 完整展示问题 + 选项（选中项 ✓ 标记）+ 历史问答对。
    if data.fold == FoldState::Collapsed {
        return (lines, None);
    }
    if !data.question.is_empty() {
        let mut spans = cont_prefix(grid, sem.text.dim);
        spans.push(Span::styled(
            truncate_by_width(&data.question, content),
            Style::default().fg(sem.text.muted),
        ));
        lines.push(Line::from(spans));
    }
    // 选项行（回答后状态可见：选中项 ✓ + success 色；横向/Narrow 布局与
    // pending 同口径——completed 不可聚焦，不产出 InteractionLayout）。
    if !data.options.is_empty() {
        let chosen_idx = data
            .result
            .as_deref()
            .and_then(|r| data.options.iter().position(|o| o == r));
        if grid.is_narrow() || data.options.len() <= 1 {
            for (i, label) in data.options.iter().enumerate() {
                let mut spans = cont_prefix(grid, sem.text.dim);
                let is_chosen = Some(i) == chosen_idx;
                let text = if is_chosen {
                    format!("[{} {label}]", sym().success)
                } else {
                    format!("[{label}]")
                };
                spans.push(Span::styled(
                    truncate_by_width(&text, content),
                    Style::default().fg(if is_chosen {
                        sem.status.success
                    } else {
                        sem.text.primary
                    }),
                ));
                lines.push(Line::from(spans));
            }
        } else {
            let text = data
                .options
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    if Some(i) == chosen_idx {
                        format!("[{} {label}]", sym().success)
                    } else {
                        format!("[{label}]")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ");
            let mut spans = cont_prefix(grid, sem.text.dim);
            spans.push(Span::styled(
                truncate_by_width(&text, content),
                Style::default().fg(sem.text.primary),
            ));
            lines.push(Line::from(spans));
        }
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
