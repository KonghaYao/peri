//! ViewModel 变体组件。
//!
//! 每个 ViewModel 变体一个 #[component]，由 MessageArea 父组件 match 分发。
//! ViewModel 类型定义见 `peri-tui/src/kit/tui_render_unit.rs`。

pub mod assistant_bubble;
pub mod collapsed_group;
pub mod reasoning_block;
pub mod subagent_group;
pub mod system_note;
pub mod tool_card;
pub mod user_bubble;
