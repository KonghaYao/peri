//! Overlay 故事：共享外框 + 宿主持有的确认/下载状态。
//!
//! 预览里的弹窗按宿主的路径绘制：先在既有矩形清屏一次（`Clear`），再由
//! `OverlayFrame` 画上下边框与标题，正文落在外框内区。点击/滚轮的分流由
//! `overlay::hit` 判定——弹窗外滚轮交给背景消息，弹窗内滚轮被遮挡。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use ratatui_kit::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use peri_theme::theme::ThemeDefinition;
use peri_tui_components::overlay::{OverlayActions, OverlayFrame, OverlayHit, actions, hit};

use super::{
    ACCENT, FG, MUTED, StoryState, Variant, WHEEL_ROWS, accent_style, muted_style, rect_contains,
};

/// 背景消息行数——弹窗外的滚轮滚动这些行（宿主消息区）。
const BACKGROUND_ROWS: usize = 24;

pub(crate) struct OverlayFixture {
    title: &'static str,
    /// 外框正文：(文本, 颜色)。
    body: &'static [(&'static str, Color)],
    width: u16,
    height: u16,
    /// 外框内点击是否执行确认（下载弹窗没有确认动作）。
    click_confirms: bool,
    confirm_label: &'static str,
}

const CONFIRM: OverlayFixture = OverlayFixture {
    title: "确认操作",
    body: &[
        ("  切换到线程「重构 TUI 组件」？", FG),
        ("  当前未提交的输入会保留。", MUTED),
        ("", FG),
    ],
    width: 46,
    height: 11,
    click_confirms: true,
    confirm_label: "confirmed",
};

const DOWNLOAD: OverlayFixture = OverlayFixture {
    title: "下载主题 3/5",
    body: &[
        ("  ✓ base.toml", Color::Green),
        ("  ✓ syntax.toml", Color::Green),
        ("  ✓ markdown.toml", Color::Green),
        ("  → statusline.toml", Color::Yellow),
        ("  · popup.toml", MUTED),
        ("", FG),
        ("  下载中… Esc 隐藏进度，后台继续", MUTED),
    ],
    width: 46,
    height: 13,
    click_confirms: false,
    confirm_label: "popup clicks",
};

pub(crate) struct OverlayState {
    fixture: &'static OverlayFixture,
    pub(crate) confirmed: usize,
    pub(crate) back: usize,
    pub(crate) outside: usize,
    pub(crate) scroll: usize,
}

impl OverlayState {
    fn new(fixture: &'static OverlayFixture) -> Self {
        Self {
            fixture,
            confirmed: 0,
            back: 0,
            outside: 0,
            scroll: 0,
        }
    }
}

fn confirm_initial() -> StoryState {
    StoryState::Overlay(OverlayState::new(&CONFIRM))
}

fn download_initial() -> StoryState {
    StoryState::Overlay(OverlayState::new(&DOWNLOAD))
}

pub(crate) const VARIANTS: &[Variant] = &[
    Variant {
        key: "confirm",
        note: "Confirm/back use the shared Button; host keeps outside clicks.",
        initial: confirm_initial,
    },
    Variant {
        key: "download",
        note: "Fixed download rows; the popup has no confirm action and still blocks the wheel inside.",
        initial: download_initial,
    },
];

fn frame_for(fixture: &OverlayFixture) -> OverlayFrame<'static> {
    OverlayFrame::new(Line::from(fixture.title))
        .border(Style::new().fg(MUTED))
        .title_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// 弹窗矩形——绘制与命中共用一次计算。状态行保留在预览最后一行。
fn popup_rect(area: Rect, fixture: &OverlayFixture) -> Rect {
    let content = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let width = fixture.width.min(content.width);
    let height = fixture.height.min(content.height);
    Rect::new(
        content.x + content.width.saturating_sub(width) / 2,
        content.y + content.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
pub(crate) fn popup_bounds(state: &OverlayState, area: Rect) -> Rect {
    popup_rect(area, state.fixture)
}

fn background_line(index: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("  transcript row {:02} · assistant output", index + 1),
        muted_style(),
    ))
}

fn story_theme() -> &'static ThemeDefinition {
    static THEME: std::sync::OnceLock<ThemeDefinition> = std::sync::OnceLock::new();
    THEME.get_or_init(peri_theme::builtin::dark_theme)
}

/// 确认/返回动作行——与真实 ConfirmPopup 共用 `overlay::actions`。
fn action_row() -> OverlayActions {
    actions("确认", "返回", story_theme())
}

/// 动作行的绝对行号与按钮列区间（相对弹窗内区左上角）。
fn action_geometry(state: &OverlayState, area: Rect) -> Option<(u16, (u16, u16))> {
    if !state.fixture.click_confirms {
        return None;
    }
    let popup = popup_rect(area, state.fixture);
    // 正文从内区左上角开始；动作行紧随 fixture 正文之后。
    let row = popup
        .y
        .saturating_add(1)
        .saturating_add(state.fixture.body.len() as u16);
    let actions = action_row();
    let x = popup.x.saturating_add(1);
    Some((
        row,
        (
            x.saturating_add(actions.back.0),
            x.saturating_add(actions.back.1),
        ),
    ))
}

#[cfg(test)]
pub(crate) fn back_button_bounds(state: &OverlayState, area: Rect) -> Option<Rect> {
    let (row, (start, end)) = action_geometry(state, area)?;
    Some(Rect::new(start, row, end.saturating_sub(start), 1))
}

fn body_lines(state: &OverlayState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = state
        .fixture
        .body
        .iter()
        .map(|(text, color)| Line::from(Span::styled(*text, Style::new().fg(*color))))
        .collect();
    if state.fixture.click_confirms {
        lines.push(action_row().line);
        lines.push(Line::from(Span::styled(
            format!("  已确认 {} 次", state.confirmed),
            accent_style(),
        )));
    }
    lines
}

fn status_line(state: &OverlayState) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "{} {} · back {} · outside clicks {} · transcript scroll {}",
            state.fixture.confirm_label, state.confirmed, state.back, state.outside, state.scroll
        ),
        muted_style(),
    ))
}

pub(crate) fn render(state: &OverlayState, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let visible = area.height as usize;
    let lines: Vec<Line<'static>> = (state.scroll..BACKGROUND_ROWS)
        .take(visible)
        .map(background_line)
        .collect();
    Paragraph::new(lines).render(area, buf);

    let popup = popup_rect(area, state.fixture);
    if popup.width > 0 && popup.height > 0 {
        // 宿主在既有矩形只清屏一次；OverlayFrame 自己不清屏。
        Clear.render(popup, buf);
        let frame = frame_for(state.fixture);
        frame.render(popup, buf);
        let inner = frame.inner(popup);
        let body = Rect::new(
            inner.x.saturating_add(1),
            inner.y,
            inner.width.saturating_sub(2),
            inner.height,
        );
        if body.width > 0 && body.height > 0 {
            Paragraph::new(body_lines(state)).render(body, buf);
        }
    }

    status_line(state).render(
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        buf,
    );
}

pub(crate) fn handle_mouse(state: &mut OverlayState, mouse: &MouseEvent, area: Rect) -> bool {
    if !rect_contains(area, mouse.column, mouse.row) {
        return false;
    }
    let boundary = hit(mouse, popup_rect(area, state.fixture));
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => match boundary {
            // 外点归宿主：故事只记账，不触发确认。
            OverlayHit::Outside => {
                state.outside += 1;
                false
            }
            OverlayHit::Border | OverlayHit::Inside => {
                if state.fixture.click_confirms {
                    let on_back =
                        action_geometry(state, area).is_some_and(|(row, (start, end))| {
                            mouse.row == row && (start..end).contains(&mouse.column)
                        });
                    if on_back {
                        state.back += 1;
                    } else {
                        state.confirmed += 1;
                    }
                }
                true
            }
        },
        // HITL 弹窗外的滚轮放行给消息区；弹窗内滚轮被遮挡。
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => match boundary {
            OverlayHit::Outside => {
                let max = BACKGROUND_ROWS.saturating_sub(area.height as usize);
                state.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    state.scroll.saturating_sub(WHEEL_ROWS)
                } else {
                    (state.scroll + WHEEL_ROWS).min(max)
                };
                true
            }
            OverlayHit::Border | OverlayHit::Inside => true,
        },
        _ => false,
    }
}
