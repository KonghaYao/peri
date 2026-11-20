//! Mermaid 终稿与流式字符图预览；播放和滚动沿用 storybook 文档控件。
//!
//! 静态 fixture 取自 mermaid 官方示例（https://mermaid.js.org/syntax/all-examples.html），
//! 只把 mermaid-text 0.58 不接受的写法做等价改写；`block-beta` 因标签解析丢字不设样例。

use peri_tui_components::mermaid::frame::{render_final, render_source};
use peri_tui_components::mermaid::stream::{MermaidStream, detect_mode};
use ratatui::style::Style;
use ratatui::text::Line;

use super::document::DocumentContent;
use super::{StoryState, accent_style, muted_style, normal_style, streaming};

const FLOWCHART: &str = include_str!("../../../mermaid/testdata/flowchart.mmd");
const SEQUENCE: &str = include_str!("../../../mermaid/testdata/sequence.mmd");
const STATE: &str = include_str!("../../../mermaid/testdata/state.mmd");
const CLASS: &str = include_str!("../../../mermaid/testdata/class.mmd");
const ER: &str = include_str!("../../../mermaid/testdata/er.mmd");
const PIE: &str = include_str!("../../../mermaid/testdata/pie.mmd");
const JOURNEY: &str = include_str!("../../../mermaid/testdata/journey.mmd");
const GANTT: &str = include_str!("../../../mermaid/testdata/gantt.mmd");
const TIMELINE: &str = include_str!("../../../mermaid/testdata/timeline.mmd");
const GITGRAPH: &str = include_str!("../../../mermaid/testdata/gitgraph.mmd");
const MINDMAP: &str = include_str!("../../../mermaid/testdata/mindmap.mmd");
const QUADRANT: &str = include_str!("../../../mermaid/testdata/quadrant.mmd");
const REQUIREMENT: &str = include_str!("../../../mermaid/testdata/requirement.mmd");
const SANKEY: &str = include_str!("../../../mermaid/testdata/sankey.mmd");
const XYCHART: &str = include_str!("../../../mermaid/testdata/xychart.mmd");
const ARCHITECTURE: &str = include_str!("../../../mermaid/testdata/architecture.mmd");
const PACKET: &str = include_str!("../../../mermaid/testdata/packet.mmd");
const STREAMING: &str = include_str!("../../../mermaid/testdata/streaming.mmd");

fn completed(source: &'static str) -> DocumentContent<'static> {
    DocumentContent::Mermaid {
        source,
        terminal: true,
    }
}

fn static_story(source: &'static str) -> StoryState {
    StoryState::Document {
        content: completed(source),
        scroll: 0,
        h_scroll: 0,
        pressed_button: None,
        h_drag: None,
        layout: None,
    }
}

pub(super) fn flowchart_initial() -> StoryState {
    static_story(FLOWCHART)
}

pub(super) fn sequence_initial() -> StoryState {
    static_story(SEQUENCE)
}

pub(super) fn state_initial() -> StoryState {
    static_story(STATE)
}

pub(super) fn class_initial() -> StoryState {
    static_story(CLASS)
}

pub(super) fn er_initial() -> StoryState {
    static_story(ER)
}

pub(super) fn pie_initial() -> StoryState {
    static_story(PIE)
}

pub(super) fn journey_initial() -> StoryState {
    static_story(JOURNEY)
}

pub(super) fn gantt_initial() -> StoryState {
    static_story(GANTT)
}

pub(super) fn timeline_initial() -> StoryState {
    static_story(TIMELINE)
}

pub(super) fn gitgraph_initial() -> StoryState {
    static_story(GITGRAPH)
}

pub(super) fn mindmap_initial() -> StoryState {
    static_story(MINDMAP)
}

pub(super) fn quadrant_initial() -> StoryState {
    static_story(QUADRANT)
}

pub(super) fn requirement_initial() -> StoryState {
    static_story(REQUIREMENT)
}

pub(super) fn sankey_initial() -> StoryState {
    static_story(SANKEY)
}

pub(super) fn xychart_initial() -> StoryState {
    static_story(XYCHART)
}

pub(super) fn architecture_initial() -> StoryState {
    static_story(ARCHITECTURE)
}

pub(super) fn packet_initial() -> StoryState {
    static_story(PACKET)
}

pub(super) fn streaming_initial() -> StoryState {
    // 固定前缀停在子图内，首帧同时展示已确认的边、作用域提示和源码尾部。
    let cut = STREAMING
        .find("    Worker --> Cache")
        .unwrap_or(STREAMING.len());
    streaming::streaming_state_at(completed(STREAMING), cut)
}

fn styled_rows<'a>(
    rows: impl Iterator<Item = &'a str>,
    style: Style,
) -> impl Iterator<Item = Line<'static>> {
    rows.map(move |row| Line::styled(row.to_owned(), style))
}

/// 文档行：字符图、源码与提示都保持渲染器给出的原始宽度，超宽部分由
/// 文档底部的横向滚动条查看；只有 Markdown 正文按列宽折行。
pub(super) fn lines(source: &str, terminal: bool, width: usize) -> Vec<Line<'static>> {
    let mut tail = "";
    let mut notice = None;
    let frame = if terminal {
        Some(render_final(source, detect_mode(source, true), width))
    } else {
        // 每次从当前前缀重建识别状态，往回拖动与直达同一位置的结果完全相同。
        let mut stream = MermaidStream::new();
        let update = stream.observe(source, false);
        notice = update.paused.map(str::to_owned).or_else(|| {
            update
                .open_scope
                .map(|scope| format!("未闭合：{}（{} 条完整语句）", scope.label, scope.statements))
        });
        if update.drawable {
            tail = &source[update.trusted_end..];
            Some(render_source(update.trusted, width))
        } else {
            notice.get_or_insert_with(|| "等待完整 Mermaid 语句".to_owned());
            None
        }
    };
    let mut rows = match frame {
        Some(Ok(frame)) => {
            let mut rows: Vec<_> =
                styled_rows(frame.lines().iter().map(String::as_str), normal_style()).collect();
            if !tail.trim().is_empty() {
                rows.extend(styled_rows(["未确认源码"].into_iter(), accent_style()));
                rows.extend(styled_rows(tail.lines(), muted_style()));
            }
            rows
        }
        Some(Err(error)) => {
            notice = Some(error.to_string());
            styled_rows(source.lines(), muted_style()).collect()
        }
        None => styled_rows(source.lines(), muted_style()).collect(),
    };
    if let Some(notice) = notice {
        rows.extend(styled_rows([notice.as_str()].into_iter(), accent_style()));
    }
    rows
}
