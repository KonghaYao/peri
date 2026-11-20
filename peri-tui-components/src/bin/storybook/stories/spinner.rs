//! Spinner 故事：固定输入的无状态帧。
//!
//! 帧序号、帧集、文案与样式都在宿主侧；点击「推进一帧」模拟宿主的时钟推进，
//! 组件每帧只做取模与拼装。空闲变体固定帧 0，点击不会让底部占位行转动。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_kit::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use peri_tui_components::spinner::Spinner;

use super::{ACCENT, MUTED, StoryState, Variant, muted_style, rect_contains};

/// footer 活动行的帧集（宿主 `animation::BRAILLE_FRAMES` 的固定副本）。
const FOOTER_FRAMES: &[char] = &[
    '✳', '✴', '✵', '✶', '✷', '✸', '✹', '✺', '✻', '✼', '❃', '❊', '✼', '✻', '✺', '✸',
];

/// 运行中工具图标的帧集（宿主 `animation::BRAILLE_LOADING_FRAMES`）。
const TOOL_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// 工具进行中符号色（宿主语义色 `status.running` 的固定替身）。
const RUNNING: Color = Color::Yellow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpinnerCase {
    /// 消息区底部空闲占位：宿主固定帧 0，不随时间转动。
    Idle,
    /// 消息区底部活动行：帧由宿主时钟推进，耗时与 token 文案已在宿主格式化。
    Active,
    /// 运行中工具图标：宿主只取字形，再拼进工具卡标题。
    Tool,
}

pub(crate) struct SpinnerState {
    pub(crate) case: SpinnerCase,
    pub(crate) frame: u64,
}

impl SpinnerState {
    fn new(case: SpinnerCase) -> Self {
        // 固定输入：dump 永远从同一帧开始。
        let frame = match case {
            SpinnerCase::Idle => 0,
            SpinnerCase::Active => 3,
            SpinnerCase::Tool => 4,
        };
        Self { case, frame }
    }
}

fn idle_initial() -> StoryState {
    StoryState::Spinner(SpinnerState::new(SpinnerCase::Idle))
}

fn active_initial() -> StoryState {
    StoryState::Spinner(SpinnerState::new(SpinnerCase::Active))
}

fn tool_initial() -> StoryState {
    StoryState::Spinner(SpinnerState::new(SpinnerCase::Tool))
}

pub(crate) const VARIANTS: &[Variant] = &[
    Variant {
        key: "idle",
        note: "Idle footer row: the host holds frame 0, so clicking never advances it.",
        initial: idle_initial,
    },
    Variant {
        key: "active",
        note: "Active footer row: click to advance the host-owned frame index.",
        initial: active_initial,
    },
    Variant {
        key: "tool",
        note: "Running tool icon: the host takes only the glyph and composes the card header.",
        initial: tool_initial,
    },
];

fn note(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), muted_style()))
}

/// 宿主组合的已格式化文字：动词（语义色）+ 可选耗时/token（muted）。
fn spinner_line(
    frames: &'static [char],
    frame: u64,
    verb: &str,
    suffix: Option<&str>,
    color: Color,
) -> Line<'static> {
    let mut spans = vec![Span::styled(verb.to_string(), Style::new().fg(color))];
    if let Some(suffix) = suffix {
        spans.push(Span::styled(format!(" ({suffix})"), Style::new().fg(MUTED)));
    }
    Spinner::new(frames, frame)
        .icon_style(Style::new().fg(color))
        .text(Line::from(spans))
        .line()
}

pub(crate) fn render(state: &SpinnerState, area: Rect, buf: &mut Buffer) {
    let lines: Vec<Line<'static>> = match state.case {
        SpinnerCase::Idle => vec![
            spinner_line(FOOTER_FRAMES, 0, "格物致知", None, MUTED),
            Line::from(""),
            note("host frame 0 · idle row is static"),
        ],
        SpinnerCase::Active => vec![
            spinner_line(
                FOOTER_FRAMES,
                state.frame,
                "思考中…",
                Some("12s · ↓ 1.5k tokens"),
                ACCENT,
            ),
            Line::from(""),
            note(format!(
                "host frame {} / {} · click advances",
                state.frame,
                FOOTER_FRAMES.len()
            )),
        ],
        SpinnerCase::Tool => {
            let glyph = Spinner::new(TOOL_FRAMES, state.frame)
                .glyph()
                .unwrap_or(' ');
            vec![
                spinner_line(TOOL_FRAMES, state.frame, "Bash  cargo test", None, RUNNING),
                Line::from(""),
                note(format!(
                    "tool glyph {} · host frame {} / {}",
                    glyph,
                    state.frame,
                    TOOL_FRAMES.len()
                )),
                note("glyph is composed by the host into the tool card header"),
            ]
        }
    };
    Paragraph::new(lines).render(area, buf);
}

pub(crate) fn handle_mouse(state: &mut SpinnerState, mouse: &MouseEvent, area: Rect) -> bool {
    if !rect_contains(area, mouse.column, mouse.row) {
        return false;
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return false;
    }
    // 空闲底部不转动；活动行由宿主时钟推进，故事用点击代替。
    if state.case != SpinnerCase::Idle {
        state.frame = state.frame.saturating_add(1);
    }
    true
}
