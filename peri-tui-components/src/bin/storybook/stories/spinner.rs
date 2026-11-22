//! Spinner 故事：固定输入的无状态帧。
//!
//! 帧序号、帧集、文案与样式都在宿主侧；点击「推进一帧」模拟宿主的时钟推进，
//! 组件每帧只做取模与拼装。空闲变体固定帧 0，点击不会让底部占位行转动。

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_kit::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use peri_tui_components::shimmer::{ShimmerStyle, shimmer_spans};
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

/// 宿主 footer / 工具动画的帧间隔（`animation.rs`：100ms/帧）。
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpinnerCase {
    /// 消息区底部空闲占位：宿主固定帧 0，不随时间转动。
    Idle,
    /// 消息区底部活动行：帧由宿主时钟推进，耗时与 token 文案已在宿主格式化。
    Active,
    /// 运行中工具图标：宿主只取字形，再拼进工具卡标题。
    Tool,
    /// 尾部折叠组的状态标签：宿主给 100ms 帧序号，标签按 2s 周期扫光。
    Shimmer,
}

pub(crate) struct SpinnerState {
    pub(crate) case: SpinnerCase,
    pub(crate) frame: u64,
    /// 上一次按宿主时钟结算的帧边界；只在 `tick_at` 中推进。
    last: Instant,
}

impl SpinnerState {
    fn new(case: SpinnerCase) -> Self {
        // 固定输入：dump 永远从同一帧开始。
        let frame = match case {
            SpinnerCase::Idle => 0,
            SpinnerCase::Active => 3,
            SpinnerCase::Tool => 4,
            // 标签扫光周期 20 帧，5 帧正好把光带放在词中间。
            SpinnerCase::Shimmer => 5,
        };
        Self {
            case,
            frame,
            last: Instant::now(),
        }
    }
}

/// 只有活动行与工具图标随时间转动；空闲占位由宿主固定帧 0。
pub(crate) fn is_animating(state: &SpinnerState) -> bool {
    state.case != SpinnerCase::Idle
}

/// 按宿主的 100ms 帧节奏推进；一次 tick 可跨多帧，余数留到下次。
pub(crate) fn tick_at(state: &mut SpinnerState, now: Instant) {
    if !is_animating(state) {
        return;
    }
    let elapsed = now.saturating_duration_since(state.last);
    let steps = (elapsed.as_millis() / FRAME_INTERVAL.as_millis()) as u64;
    if steps == 0 {
        return;
    }
    state.frame = state.frame.saturating_add(steps);
    state.last += FRAME_INTERVAL * u32::try_from(steps).unwrap_or(u32::MAX);
}

#[cfg(test)]
impl SpinnerState {
    pub(crate) fn last_tick(&self) -> Instant {
        self.last
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

fn shimmer_initial() -> StoryState {
    StoryState::Spinner(SpinnerState::new(SpinnerCase::Shimmer))
}

pub(crate) const VARIANTS: &[Variant] = &[
    Variant {
        key: "idle",
        note: "Idle footer row: the host holds frame 0, so clicking never advances it.",
        initial: idle_initial,
    },
    Variant {
        key: "active",
        note: "Active footer row: the host clock advances one frame every 100ms; click also advances.",
        initial: active_initial,
    },
    Variant {
        key: "tool",
        note: "Running tool icon: the host rotates the glyph every 100ms and composes the card header.",
        initial: tool_initial,
    },
    Variant {
        key: "shimmer",
        note: "Collapsed-group Working label: the host supplies the 100ms frame index; the band sweeps in 2s.",
        initial: shimmer_initial,
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
                "host frame {} / {} · click also advances",
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
        SpinnerCase::Shimmer => {
            let mut spans = vec![
                Span::styled("▸ ", Style::new().fg(MUTED)),
                Span::styled("Read 2", Style::new().fg(MUTED)),
                Span::styled(" · ", Style::new().fg(MUTED)),
            ];
            spans.extend(shimmer_spans(
                "Working",
                state.frame.saturating_mul(100),
                ShimmerStyle {
                    base: RUNNING,
                    highlight: Color::White,
                    truecolor: true,
                    animated: true,
                },
            ));
            vec![
                Line::from(spans),
                Line::from(""),
                note(format!(
                    "collapsed group tail · host frame {} · label phase {}ms",
                    state.frame,
                    state.frame.saturating_mul(100)
                )),
                note(
                    "truecolor blends base → highlight + BOLD; without RGB it uses DIM / plain / BOLD",
                ),
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
