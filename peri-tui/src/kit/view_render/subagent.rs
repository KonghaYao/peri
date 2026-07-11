//! SubAgent 组渲染（嵌套子 Agent 内联展示）。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
use crate::kit::view_render::probe::lookup_subagent_status;
use crate::kit::view_render::tool_card::with_message_spacing;
use peri_theme::atoms::THEME_ATOM;

pub(crate) fn render_subagent_group(data: &TuiSubAgentGroup, width: usize) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;

    // 查询运行时状态（v2 DTO 缺失字段由 status probe 注入）
    let status = lookup_subagent_status(&data.agent_id);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // 子内容来源优先级：
    // 1. v2 DTO `view_models`（ACP 层填充，当前永久为空 placeholder）
    // 2. status probe 的 `recent_messages`（app 层填充）
    let children: Vec<TuiRenderUnit> = if !data.view_models.is_empty() {
        data.view_models.iter().cloned().collect()
    } else if let Some(ref s) = status {
        s.recent_messages.clone()
    } else {
        Vec::new()
    };

    // 折叠摘要：TuiToolCard 超过 5 个时，前 N-5 个渲染为单行 "▶ N collapsed tools"，
    // 最后 5 个正常渲染。非 TuiToolCard 子消息始终正常渲染。
    let tool_count = children
        .iter()
        .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
        .count();
    let collapse_count = tool_count.saturating_sub(5);
    let mut tool_idx = 0;

    if collapse_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("  \u{25b6} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("{} collapsed tools", collapse_count),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    for inner_vm in &children {
        if matches!(inner_vm, TuiRenderUnit::TuiAssistantBubble(_)) {
            continue;
        }
        if matches!(inner_vm, TuiRenderUnit::TuiToolCard(_)) {
            tool_idx += 1;
            // 跳过被折叠的前 N-5 个 TuiToolCard
            if tool_idx <= collapse_count {
                continue;
            }
        }
        let inner_segments = super::render_v2_vm(inner_vm, width);
        let inner_lines = super::segments_to_lines(&inner_segments);
        // SubAgent done 后过滤 ⎿ 输出行；running 时保留（显示进度）
        let is_running = status.as_ref().map_or(data.is_running, |s| s.is_running);
        let inner_lines: Vec<_> =
            if !is_running && matches!(inner_vm, TuiRenderUnit::TuiToolCard(_)) {
                inner_lines
                    .into_iter()
                    .filter(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        !text.contains("\u{23bf}")
                    })
                    .collect()
            } else {
                inner_lines
            };
        if inner_lines.is_empty() {
            continue;
        }
        // SubAgent 展开区内移除嵌套消息的 leading/trailing 空行
        let start = inner_lines
            .iter()
            .position(|l| !l.spans.is_empty())
            .unwrap_or(0);
        let end = inner_lines
            .iter()
            .rposition(|l| !l.spans.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        let trimmed = &inner_lines[start..end];
        for line in trimmed {
            let mut new_spans = vec![Span::raw("  ")];
            new_spans.extend(line.spans.iter().cloned());
            lines.push(Line::from(new_spans));
        }
    }

    // 显示 final_result 摘要（如果完成且有结果，最多前 3 行）
    if let Some(ref s) = status {
        if !s.is_running {
            if let Some(ref result) = s.final_result {
                let color = if s.is_error {
                    semantic.status.error
                } else {
                    semantic.text.muted
                };
                let preview_lines: Vec<&str> = result
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect();
                for line_text in preview_lines {
                    let truncated: String = line_text.chars().take(80).collect();
                    if !truncated.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
                            Span::styled(truncated, Style::default().fg(color)),
                        ]));
                    }
                }
            }
        }
    }

    with_message_spacing(lines)
}
