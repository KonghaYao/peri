//! Mermaid 终稿与流式字符图预览；播放和滚动沿用 storybook 文档控件。

use peri_tui_components::mermaid::frame::{render_final, render_source};
use peri_tui_components::mermaid::stream::{MermaidStream, detect_mode};
use peri_tui_components::text::wrap_by_width;
use ratatui::style::Style;
use ratatui::text::Line;

use super::document::DocumentContent;
use super::{StoryState, accent_style, muted_style, normal_style, streaming};

const FLOWCHART: &str = include_str!("../../../mermaid/testdata/flowchart.mmd");
const SEQUENCE: &str = include_str!("../../../mermaid/testdata/sequence.mmd");
const COLLABORATION: &str = include_str!("../../../mermaid/testdata/collaboration.mmd");
const DENSE: &str = include_str!("../../../mermaid/testdata/dense.mmd");
const LONG_WORKFLOW: &str = include_str!("../../../mermaid/testdata/long_workflow.mmd");

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
        pressed_button: None,
        layout: None,
    }
}

pub(super) fn flowchart_initial() -> StoryState {
    static_story(FLOWCHART)
}

pub(super) fn sequence_initial() -> StoryState {
    static_story(SEQUENCE)
}

pub(super) fn collaboration_initial() -> StoryState {
    static_story(COLLABORATION)
}

pub(super) fn dense_initial() -> StoryState {
    static_story(DENSE)
}

pub(super) fn long_workflow_initial() -> StoryState {
    static_story(LONG_WORKFLOW)
}

pub(super) fn streaming_initial() -> StoryState {
    // 固定前缀停在子图内，首帧同时展示已确认的边、作用域提示和源码尾部。
    streaming::streaming_state(completed(FLOWCHART), 73)
}

fn styled_rows<'a>(
    rows: impl Iterator<Item = &'a str>,
    width: usize,
    style: Style,
) -> impl Iterator<Item = Line<'static>> {
    rows.flat_map(move |row| {
        wrap_by_width(row, width)
            .into_iter()
            .map(move |text| Line::styled(text, style))
    })
}

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
            let mut rows: Vec<_> = styled_rows(
                frame.lines().iter().map(String::as_str),
                width,
                normal_style(),
            )
            .collect();
            if !tail.trim().is_empty() {
                rows.extend(styled_rows(
                    ["未确认源码"].into_iter(),
                    width,
                    accent_style(),
                ));
                rows.extend(styled_rows(tail.lines(), width, muted_style()));
            }
            rows
        }
        Some(Err(error)) => {
            notice = Some(error.to_string());
            styled_rows(source.lines(), width, muted_style()).collect()
        }
        None => styled_rows(source.lines(), width, muted_style()).collect(),
    };
    if let Some(notice) = notice {
        rows.extend(styled_rows(
            [notice.as_str()].into_iter(),
            width,
            accent_style(),
        ));
    }
    rows
}
