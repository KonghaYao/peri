//! 消息区域 ratatui-kit #[component]。
//!
//! 复用 `render/view_render.rs` 的 `render_v2_vm` 纯函数渲染全部 7 种 ViewModel 变体。
//!
//! ## I18-B：消息流滚动接入
//!
//! 历史问题：原版本中 layout 传入 `scroll_offset: u16` prop 但本组件从不消费，
//! 且没有任何按键写入 SCROLL_OFFSET atom——用户无法滚动长输出。
//!
//! 修复：本组件自管 `ScrollViewState`（ratatui-kit 0.6），并注册
//! Ctrl+Up/Ctrl+Down/Ctrl+Home/Ctrl+End 滚动键。之所以选 Ctrl+ 修饰符，
//! 是因为：
//! - Up/Down/Home/End 已被 InputArea 用于 history + 行内导航
//! - PageUp/PageDown 被 CLAUDE.md 规则禁用
//! - 鼠标滚轮在终端上跨平台支持不一致
//!
//! `use_local_events` 是广播式（不消费事件），所以 Ctrl+Up 也会传给 InputArea，
//! 但 InputArea 的 match 中无对应分支，自然忽略——安全。

#![allow(clippy::needless_update)]

use crate::kit::theme;
use crate::kit::view_render;
use peri_acp_types::view_model::ViewModel;
use ratatui_kit::{
    components::scroll_view::{ScrollBars, ScrollView, ScrollViewState},
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// MessageArea 组件 Props——由父组件（layout.rs SessionColumn）注入。
#[derive(Default, Props)]
pub struct MessageAreaProps {
    /// 已提交（committed）消息列表。
    pub view_models: Vec<ViewModel>,
    /// 当前轮次正在进行的消息列表。
    pub current_turn: Vec<ViewModel>,
    /// 是否正在加载（Agent 思考中）。
    pub loading: bool,
    /// 终端可用宽度，用于 markdown 折行。
    pub width: usize,
    /// I19-B：diff 视图展开开关（Ctrl+O toggle）。
    pub diff_visible: bool,
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut all_lines: Vec<Line<'static>> = Vec::new();

    // 已提交消息
    for vm in &props.view_models {
        let lines = view_render::render_v2_vm(vm, props.width, props.diff_visible);
        all_lines.extend(lines);
        all_lines.push(Line::from(""));
    }

    // 当前轮次消息
    for vm in &props.current_turn {
        let lines = view_render::render_v2_vm(vm, props.width, props.diff_visible);
        all_lines.extend(lines);
        all_lines.push(Line::from(""));
    }

    // 加载指示器
    if props.loading {
        all_lines.push(Line::from(vec![Span::styled(
            "● Thinking...",
            Style::default().fg(theme::LOADING),
        )]));
    }

    let paragraph = if all_lines.is_empty() {
        Paragraph::new(
            Line::from("Start a conversation...")
                .centered()
                .fg(theme::MUTED),
        )
    } else {
        Paragraph::new(all_lines)
    };

    // I18-B：手动管理模式——本组件完全掌控滚动状态，避免与 InputArea 的 Up/Down 冲突。
    // ScrollView 自动模式会监听 Up/Down/j/k/PageUp/PageDown/Home/End，与 InputArea 的
    // history 导航和行内 Home/End 严重冲突，因此必须手动。
    let scroll_state = hooks.use_state(ScrollViewState::default);

    hooks.use_local_events({
        move |event| {
            let Event::Key(key) = event else {
                return;
            };
            if key.kind != KeyEventKind::Press {
                return;
            }
            // 仅 Ctrl+ 方向键 / Home / End 触发滚动（避开 InputArea 的无修饰键绑定）
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                return;
            }
            // 排除 Ctrl+Shift+ 等组合（仅纯 Ctrl+ 触发）
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                return;
            }
            let mut state = scroll_state.write();
            match key.code {
                KeyCode::Up => state.scroll_up(),
                KeyCode::Down => state.scroll_down(),
                KeyCode::Home => state.scroll_to_top(),
                KeyCode::End => state.scroll_to_bottom(),
                _ => {}
            }
        }
    });

    element!(
        ScrollView(
            scroll_view_state: scroll_state,
            scroll_bars: ScrollBars::default(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: paragraph)
        }
    )
}
