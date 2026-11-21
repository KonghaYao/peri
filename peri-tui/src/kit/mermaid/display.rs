//! 把预览状态转成消息区行；源码始终是可回退的最终事实。

use std::time::Instant;

use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::h_viewport::{HorizontalViewport, content_width};
use peri_tui_components::scrollbar::{HorizontalBar, horizontal_scrollbar_line};
use peri_tui_components::text::slice_by_width;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};

use crate::i18n;
use crate::kit::markdown::{MermaidFence, MermaidFenceKey, MermaidMessageRef, MermaidSegment};
use crate::kit::tui_render_unit::TuiRenderUnit;

use super::hscroll;
use super::preview::{PreviewContent, PreviewNotice, rebuild_history};
use super::store;

/// 帧的横向视口元数据：绘制滚动条与命中换算共用同一组数字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MermaidHView {
    pub(crate) viewport: usize,
    pub(crate) max_offset: usize,
    /// 帧行数（含滚动条行），用于确定可横向滚动的屏幕行范围。
    pub(crate) rows: usize,
    /// 滚动条行相对围栏首行的逻辑偏移；`None` 表示视口太窄放不下条。
    pub(crate) bar_line: Option<usize>,
    pub(crate) bar: Option<HorizontalBar>,
}

/// 一处围栏的渲染结果：行 + 横向视口。
pub(crate) struct FenceRender {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) h_view: Option<MermaidHView>,
}

/// 一处围栏在消息行里的位置与调度信息。
#[derive(Debug, Clone)]
pub(crate) struct FenceSpan {
    pub(crate) key: MermaidFenceKey,
    pub(crate) fence: std::sync::Arc<MermaidFence>,
    pub(crate) width: usize,
    pub(crate) frozen: bool,
    /// 消息内逻辑行范围（wrap 前，end 为开区间）。
    pub(crate) logical: std::ops::Range<usize>,
    pub(crate) layout_bytes: usize,
    /// 正文前缀宽度（围栏首列在消息行内的偏移），横向条命中用。
    pub(crate) prefix: usize,
    pub(crate) h_view: Option<MermaidHView>,
}

impl FenceSpan {
    /// 稳定块内的局部范围 → 整条消息的绝对范围。
    #[must_use]
    pub(crate) fn shifted(&self, delta: usize) -> Self {
        Self {
            key: self.key.clone(),
            fence: std::sync::Arc::clone(&self.fence),
            width: self.width,
            frozen: self.frozen,
            logical: self.logical.start.saturating_add(delta)
                ..self.logical.end.saturating_add(delta),
            layout_bytes: self.layout_bytes,
            prefix: self.prefix,
            h_view: self.h_view,
        }
    }
}

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
    pub(crate) fn message_identity(&self) -> crate::kit::markdown::MermaidMessageIdentity {
        crate::kit::markdown::MermaidMessageIdentity {
            session_id: self.session_id.clone(),
            message: self.message.clone(),
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
/// 直接渲染入口（测试/嵌套渲染）使用；消息区走 [`display_lines_peek`]。
pub(crate) fn display_lines(
    segment: &MermaidSegment,
    scope: &PreviewScope,
    width: usize,
    frozen: bool,
    foreground: Color,
) -> FenceRender {
    let key = scope.key(segment.fence.open_start);
    let selected = super::action_selected(&key);
    if frozen && !store::worker_running() {
        // 无后台线程（CLI/测试）时，历史消息直接按同一纯函数重建，
        // 避免留下永不发布的待排版任务。
        let replay = rebuild_history(&segment.fence, width, None);
        return render_content(
            segment,
            &key,
            replay.content,
            replay.notice,
            width,
            foreground,
            selected,
        );
    }
    let view = store::observe(key.clone(), &segment.fence, width, frozen);
    render_content(
        segment,
        &key,
        view.content,
        view.notice,
        width,
        foreground,
        selected,
    )
}

/// 消息区渲染路径：只读取当前帧，不排队。可见性调度由 transcript 用围栏
/// 视觉范围与视口相交后统一发起，避免离屏消息触发排版。
pub(crate) fn display_lines_peek(
    segment: &MermaidSegment,
    scope: &PreviewScope,
    width: usize,
    _frozen: bool,
    foreground: Color,
) -> (FenceRender, usize) {
    let key = scope.key(segment.fence.open_start);
    let view = store::peek(key.clone(), &segment.fence, width);
    let bytes = content_bytes(&view.content);
    let selected = super::action_selected(&key);
    (
        render_content(
            segment,
            &key,
            view.content,
            view.notice,
            width,
            foreground,
            selected,
        ),
        bytes,
    )
}

fn content_bytes(content: &PreviewContent) -> usize {
    match content {
        PreviewContent::Source => 0,
        PreviewContent::Frame { layout_bytes, .. } => *layout_bytes,
    }
}

fn render_content(
    segment: &MermaidSegment,
    key: &MermaidFenceKey,
    content: PreviewContent,
    notice: Option<PreviewNotice>,
    width: usize,
    foreground: Color,
    action_selected: bool,
) -> FenceRender {
    let mut h_view = None;
    let mut lines = match content {
        PreviewContent::Source => segment.lines.clone(),
        PreviewContent::Frame { lines, tail, .. } => {
            let viewport = HorizontalViewport::new(width, content_width(&lines));
            // 不溢出时也要读一次：`offset_for` 在该分支返回 0 并清掉旧列，
            // 否则帧变窄再变宽会从陈旧列恢复。
            let offset = hscroll::offset_for(key, viewport.viewport(), viewport.content());
            let mut rendered = frame_lines(&lines, foreground, viewport, offset);
            let bar = viewport.bar(u16::try_from(width).unwrap_or(u16::MAX), offset);
            let bar_line = bar.map(|bar| {
                let index = rendered.len();
                rendered.push(horizontal_bar_row(bar, width));
                index
            });
            if viewport.overflows() {
                h_view = Some(MermaidHView {
                    viewport: viewport.viewport(),
                    max_offset: viewport.max_offset(),
                    rows: rendered.len(),
                    bar_line,
                    bar,
                });
            }
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
    lines.push(open_action_line(foreground, action_selected));
    FenceRender { lines, h_view }
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

fn frame_lines(
    lines: &[String],
    foreground: Color,
    viewport: HorizontalViewport,
    offset: usize,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            let text = if viewport.overflows() {
                slice_by_width(line, offset, viewport.viewport())
            } else {
                line.clone()
            };
            if text.is_empty() {
                Line::default()
            } else {
                Line::from(Span::styled(text, Style::default().fg(foreground)))
            }
        })
        .collect()
}

/// 帧底部的横向滚动条行；轨道与滑块由组件 `HorizontalBar` 决定，绘制与命中同源。
fn horizontal_bar_row(bar: HorizontalBar, width: usize) -> Line<'static> {
    let semantic = THEME_ATOM.state().read().semantic;
    let thumb = Style::default().fg(semantic.text.dim).bg(semantic.text.dim);
    let arrow = Style::default()
        .fg(semantic.text.muted)
        .add_modifier(Modifier::BOLD);
    horizontal_scrollbar_line(u16::try_from(width).unwrap_or(u16::MAX), &bar, thumb, arrow)
}

fn open_action_line(foreground: Color, selected: bool) -> Line<'static> {
    let text = if selected {
        format!("▸ [{}]", i18n::tr("mermaid-open-diagram"))
    } else {
        format!("▸ {}", i18n::tr("mermaid-open-diagram"))
    };
    let style = if selected {
        Style::default()
            .fg(foreground)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
            .fg(foreground)
            .add_modifier(Modifier::DIM | Modifier::UNDERLINED)
    };
    Line::from(Span::styled(text, style))
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
        PreviewNotice::Unsupported(reason) => i18n::tr_args(
            "mermaid-unsupported",
            &[("reason".to_string(), reason.into())],
        ),
        PreviewNotice::TooLarge { bytes } => i18n::tr_args(
            "mermaid-too-large",
            &[("bytes".to_string(), bytes.to_string().into())],
        ),
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
