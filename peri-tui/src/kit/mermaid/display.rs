//! 把预览状态转成消息区行；源码始终是可回退的最终事实。

use std::time::Instant;

use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};

use crate::i18n;
use crate::kit::markdown::{MermaidFenceKey, MermaidMessageRef, MermaidSegment};
use crate::kit::tui_render_unit::TuiRenderUnit;

use super::preview::{PreviewContent, PreviewNotice, rebuild_history};
use super::store;

/// 一处围栏的稳定身份：会话 + 消息 + 原文位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewScope {
    session_id: String,
    message: MermaidMessageRef,
}

impl PreviewScope {
    #[must_use]
    pub(crate) fn for_bubble(
        session_id: impl Into<String>,
        message_id: Option<&str>,
        started_at: Option<Instant>,
        content_hash: u64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            message: MermaidMessageRef::for_bubble(message_id, started_at, content_hash),
        }
    }

    #[must_use]
    pub(crate) fn key(&self, open_start: usize) -> MermaidFenceKey {
        MermaidFenceKey {
            message: crate::kit::markdown::MermaidMessageIdentity {
                session_id: self.session_id.clone(),
                message: self.message.clone(),
            },
            open_start,
        }
    }
}

/// 消息区展示行：有可信帧时显示图 + 未确认源码尾部；否则保持完整源码。
pub(crate) fn display_lines(
    segment: &MermaidSegment,
    scope: &PreviewScope,
    width: usize,
    frozen: bool,
    foreground: Color,
) -> Vec<Line<'static>> {
    if frozen && !store::worker_running() {
        // 无后台线程（CLI/测试）时，历史消息直接按同一纯函数重建，
        // 避免留下永不发布的待排版任务。
        let replay = rebuild_history(&segment.fence, width, None);
        return render_content(segment, replay.content, replay.notice, foreground);
    }
    let view = store::observe(
        scope.key(segment.fence.open_start),
        &segment.fence,
        width,
        frozen,
    );
    render_content(segment, view.content, view.notice, foreground)
}

fn render_content(
    segment: &MermaidSegment,
    content: PreviewContent,
    notice: Option<PreviewNotice>,
    foreground: Color,
) -> Vec<Line<'static>> {
    let mut lines = match content {
        PreviewContent::Source => segment.lines.clone(),
        PreviewContent::Frame { lines, tail } => {
            let mut rendered = frame_lines(&lines, foreground);
            if !tail.is_empty() {
                rendered.push(Line::default());
                for line in tail.lines() {
                    rendered.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(foreground),
                    )));
                }
            }
            rendered
        }
    };
    if let Some(notice) = notice {
        lines.push(notice_line(notice, foreground));
    }
    lines
}

/// 只做一次低成本判断，避免预览版本变化时重算无关消息。
#[must_use]
pub(crate) fn vm_might_have_mermaid(vm: &TuiRenderUnit) -> bool {
    let TuiRenderUnit::TuiAssistantBubble(bubble) = vm else {
        return false;
    };
    let text = bubble.text.as_str();
    if !text.contains("mermaid") && !text.contains("Mermaid") && !text.contains("MERMAID") {
        return false;
    }
    text.lines().any(|line| {
        let mut trimmed = line.trim_start();
        while let Some(rest) = trimmed.strip_prefix('>') {
            trimmed = rest.trim_start();
        }
        let info = trimmed
            .strip_prefix("```")
            .or_else(|| trimmed.strip_prefix("~~~"));
        info.and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|tag| tag.eq_ignore_ascii_case("mermaid"))
    })
}

fn frame_lines(lines: &[String], foreground: Color) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                Line::default()
            } else {
                Line::from(Span::styled(line.clone(), Style::default().fg(foreground)))
            }
        })
        .collect()
}

fn notice_line(notice: PreviewNotice, foreground: Color) -> Line<'static> {
    let text = match notice {
        PreviewNotice::Unclosed => i18n::tr("mermaid-unclosed"),
        PreviewNotice::Interrupted(reason) => i18n::tr_args(
            "mermaid-interrupted",
            &[("reason".to_string(), reason.into())],
        ),
        PreviewNotice::Failed(reason) => i18n::tr_args(
            "mermaid-render-failed",
            &[("reason".to_string(), reason.into())],
        ),
        PreviewNotice::UnknownStatement => i18n::tr("mermaid-unknown-statement"),
        PreviewNotice::OpenScope { label, statements } => i18n::tr_args(
            "mermaid-open-scope",
            &[
                ("label".to_string(), label.into()),
                ("count".to_string(), statements.to_string().into()),
            ],
        ),
    };
    Line::from(Span::styled(
        text,
        Style::default().fg(foreground).add_modifier(Modifier::DIM),
    ))
}

#[cfg(test)]
#[path = "display_test.rs"]
mod tests;
