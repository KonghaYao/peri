//! MessageArea：消息流渲染区。
//!
//! 复用 `render/view_render.rs` 的 `render_v2_vm` 纯函数渲染全部 7 种 ViewModel 变体。
//!
//! ## I18-B：消息流滚动接入
//!
//! 历史问题：原版本中 layout 传入 `scroll_offset: u16` prop 但本组件从不消费，
//! 且没有任何按键写入 SCROLL_OFFSET atom——用户无法滚动长输出。
//!
//! 修复：本组件自管 `ScrollViewState`（ratatui-kit 0.7 可控滚动模式），并注册
//! Ctrl+Up/Ctrl+Down/Ctrl+Home/Ctrl+End 滚动键。之所以选 Ctrl+ 修饰符，
//! 是因为：
//! - Up/Down/Home/End 已被 InputArea 用于 history + 行内导航
//! - PageUp/PageDown 被 CLAUDE.md 规则禁用
//! - 鼠标滚轮在终端上跨平台支持不一致
//!
//! 事件通过 `use_event_handler(EventScope::Current)` 注册，避免旧的
//! 广播式局部事件注册方式。

#![allow(clippy::needless_update)]

use crate::kit::focus_router;
use crate::kit::theme;
use crate::kit::view_render;
use crate::kit::welcome::Welcome;
use peri_acp_types::view_model::ViewModel;
use ratatui_kit::{
    components::scroll_view::{ScrollBars, ScrollView, ScrollViewState},
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::Style,
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::sync::Arc;

/// MessageArea 组件 Props——由父组件（layout.rs SessionColumn）注入。
///
/// I20-B：view_models / current_turn 改 `Arc<[ViewModel]>`——避免 layout
/// 每次 clone 快照时 O(n) 拷贝整条消息历史。
#[derive(Default, Props)]
pub struct MessageAreaProps {
    /// 已提交（committed）消息列表。
    pub view_models: Arc<[ViewModel]>,
    /// 当前轮次正在进行的消息列表。
    pub current_turn: Arc<[ViewModel]>,
    /// 是否正在加载（Agent 思考中）。
    pub loading: bool,
    /// 终端可用宽度，用于 markdown 折行。
    pub width: usize,
    /// I19-B：diff 视图展开开关（Ctrl+O toggle）。
    pub diff_visible: bool,
}

fn visual_content_height(lines: &[Line<'static>], width: usize) -> u16 {
    let width = width.max(1);
    let rows = lines.iter().fold(0usize, |sum, line| {
        let line_width = line.width().max(1);
        sum.saturating_add(line_width.div_ceil(width))
    });
    rows.max(1).min(u16::MAX as usize) as u16
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut all_lines: Vec<Line<'static>> = Vec::new();

    // 已提交消息
    for vm in props.view_models.iter() {
        let lines = view_render::render_v2_vm(vm, props.width, props.diff_visible);
        all_lines.extend(lines);
        all_lines.push(Line::from(""));
    }

    // 当前轮次消息
    for vm in props.current_turn.iter() {
        let lines = view_render::render_v2_vm(vm, props.width, props.diff_visible);
        all_lines.extend(lines);
        all_lines.push(Line::from(""));
    }

    // 加载指示器（S16：遵循 TUI-PAGE.md 2.2——◜ 思考中… 样式）
    if props.loading {
        let semantic = theme::semantic();
        all_lines.push(Line::from(vec![Span::styled(
            "◜ 思考中…",
            Style::default().fg(semantic.status.running),
        )]));
    }

    let content_height = visual_content_height(&all_lines, props.width);
    tracing::info!(
        content_height,
        line_count = all_lines.len(),
        render_width = props.width,
        "message area content metrics"
    );
    let paragraph = if all_lines.is_empty() {
        None
    } else {
        Some(Paragraph::new(all_lines))
    };

    // 消息区只吃鼠标滚轮；普通 Up/Down/Home/End 全部留给 InputArea。
    // Ctrl+ 导航键用于驱动消息区滚动，保持输入区多行/历史行为不变。
    let scroll_state = hooks.use_state(ScrollViewState::default);

    // I23-b：智能跟随——用户手动滚上后不再抢滚动，新 turn 自动恢复。
    let mut auto_scroll = hooks.use_state(|| true);

    // I23-b：智能跟随——新 turn 自动启用，用户 Ctrl+Up 滚上去后暂停。
    // deps 用 (Arc指针, len) 元组：指针变 = 新 chunk，len 变 = turn 边界。
    let current_turn_ptr = Arc::as_ptr(&props.current_turn) as *const () as usize;
    let current_turn_len = props.current_turn.len();
    let had_content = hooks.use_state(|| false);
    let current_turn_empty = props.current_turn.is_empty();
    hooks.use_effect(
        {
            let mut auto_scroll = auto_scroll;
            let mut had_content = had_content;
            move || {
                // 新 turn 开始（上一帧空→本帧非空）→ 恢复自动滚动
                if !had_content.get() && !current_turn_empty {
                    auto_scroll.set(true);
                }
                had_content.set(!current_turn_empty);

                if auto_scroll.get() {
                    scroll_state.write().scroll_to_bottom();
                }
            }
        },
        (current_turn_ptr, current_turn_len),
    );

    hooks.use_event_handler(
        EventScope::Global,
        EventPriority::High,
        move |event| match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let _ = focus_router::message_accepts_key(&key);
                let key_event = Event::Key(key);
                match key.code {
                    KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                        let is_scroll_up = matches!(key.code, KeyCode::Up | KeyCode::Home);
                        let is_scroll_end = matches!(key.code, KeyCode::End);
                        if is_scroll_up {
                            auto_scroll.set(false);
                        }
                        if is_scroll_end {
                            auto_scroll.set(true);
                        }
                        let before = scroll_state.read().offset();
                        {
                            let mut state = scroll_state.write();
                            state.handle_event(&key_event);
                        }
                        let after = scroll_state.read().offset();
                        tracing::info!(
                            ?before,
                            ?after,
                            ?key,
                            auto_scroll = auto_scroll.get(),
                            "message area handled ctrl key scroll"
                        );
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            }
            Event::Mouse(mouse) => {
                tracing::info!(?mouse, "message area mouse event");
                match mouse.kind {
                    MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight => {
                        auto_scroll.set(false);
                        let before = scroll_state.read().offset();
                        {
                            let mut state = scroll_state.write();
                            state.handle_event(&Event::Mouse(mouse));
                        }
                        let after = scroll_state.read().offset();
                        tracing::info!(?before, ?after, "message area handled mouse scroll");
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            }
            Event::Key(key) => {
                let _ = focus_router::message_accepts_key(&key);
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        },
    );

    element!(
        ScrollView(
            scroll_view_state: scroll_state,
            scroll_bars: ScrollBars::default(),
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
            disabled: true,
        ) {
            {
                if let Some(paragraph) = paragraph {
                    element!(
                        View(
                            width: Constraint::Fill(1),
                            height: Constraint::Length(content_height),
                        ) {
                            Text(text: paragraph)
                        }
                    )
                    .into_any()
                } else {
                    element!(
                        View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                            Welcome(width: props.width)
                        }
                    )
                    .into_any()
                }
            }
        }
    )
}
