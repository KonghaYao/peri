//! AI 助手消息气泡——使用 ratatui-kit-markdown Markdown 组件渲染。
//!
//! 可选包含 reasoning 块（推理过程的缩略预览）。

use std::sync::Arc;

use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};
use ratatui_kit_markdown::Markdown;

use crate::kit::bubbles::reasoning_block::ReasoningBlock;

/// AI 助手消息气泡属性。
#[derive(Props, Default)]
pub struct AssistantBubbleProps {
    /// AI 回复的 markdown 文本内容。
    pub content: Arc<str>,
    /// 可选的推理块（文本 + 折叠状态）。
    pub reasoning: Option<(Arc<str>, bool)>,
}

#[component]
pub fn AssistantBubble(props: &AssistantBubbleProps) -> impl Into<AnyElement<'static>> {
    let reasoning_element: Option<AnyElement<'static>> =
        props.reasoning.as_ref().map(|(text, collapsed)| {
            element! {
                ReasoningBlock(text: text.clone(), collapsed: *collapsed)
            }
            .into_any()
        });

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
        ) {
            { reasoning_element }
            Markdown(content: props.content.as_ref().to_string())
        }
    }
}
