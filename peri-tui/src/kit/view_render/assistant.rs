//! Assistant 消息气泡渲染（含 reasoning block）。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::i18n;
use crate::kit::markdown::MarkdownSegment;
use crate::kit::tui_render_unit::TuiReasoningBlock;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

/// 渲染 Assistant 气泡（含 reasoning + 文本）。
pub(crate) fn render_assistant_bubble(
    data: &crate::kit::tui_render_unit::TuiAssistantBubble,
    width: usize,
) -> Vec<MarkdownSegment> {
    let mut segments: Vec<MarkdownSegment> = Vec::new();

    if let Some(ref reasoning) = data.reasoning {
        segments.push(MarkdownSegment::Text(render_reasoning_block(reasoning)));
    }

    if !data.text.is_empty() {
        let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
        let palette_guard = palette_state.read();
        let blocks = crate::kit::markdown::parse_markdown(&data.text, width, *palette_guard);
        segments.extend(blocks);
    }

    segments
}

/// 推理块渲染：显示 "Thought for N chars" + 尾部 3 行预览。
pub(crate) fn render_reasoning_block(reasoning: &TuiReasoningBlock) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "render-thought-for",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        ),
        Style::default().fg(semantic.text.dim),
    )]));

    // 尾部预览（最后 3 行）
    if !reasoning.collapsed {
        let tail_lines: Vec<&str> = reasoning.text.lines().rev().take(3).collect();
        for tail in tail_lines.into_iter().rev() {
            if !tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" \u{23bf} ", Style::default().fg(semantic.text.dim)),
                    Span::styled(tail.to_string(), Style::default().fg(semantic.text.dim)),
                ]));
            }
        }
    }
    lines.push(Line::from(""));

    lines
}
