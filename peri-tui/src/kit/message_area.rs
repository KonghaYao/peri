//! 消息区域 ratatui-kit #[component]。
//!
//! Phase 4：将占位 Widget 转换为完整 ratatui-kit 组件，
//! 复用 `render/view_render.rs` 的 `render_v2_vm` 纯函数渲染全部 7 种 ViewModel 变体。

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use peri_acp_types::view_model::ViewModel;
use crate::render::view_render;
use crate::ui::theme;

/// MessageArea 组件 Props——由父组件（layout.rs SessionColumn）注入。
#[derive(Default, Props)]
pub struct MessageAreaProps {
    /// 已提交（committed）消息列表。
    pub view_models: Vec<ViewModel>,
    /// 当前轮次正在进行的消息列表。
    pub current_turn: Vec<ViewModel>,
    /// 滚动偏移量。
    pub scroll_offset: u16,
    /// 是否正在加载（Agent 思考中）。
    pub loading: bool,
    /// 终端可用宽度，用于 markdown 折行。
    pub width: usize,
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, _hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut all_lines: Vec<Line<'static>> = Vec::new();

    // 已提交消息
    for vm in &props.view_models {
        let lines = view_render::render_v2_vm(vm, props.width, false);
        all_lines.extend(lines);
        all_lines.push(Line::from(""));
    }

    // 当前轮次消息
    for vm in &props.current_turn {
        let lines = view_render::render_v2_vm(vm, props.width, false);
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

    element!(
        ScrollView(
            scroll_bars: ScrollBars::default(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: paragraph)
        }
    )
}
