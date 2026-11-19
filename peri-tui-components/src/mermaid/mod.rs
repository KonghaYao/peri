//! Mermaid 源确认、排版、语义概览与流式预览状态机。
//!
//! 只接收已确认的源码前缀与闭合状态；围栏扫描、消息调度、主题与 i18n 由调用方
//! 持有。`stream/` 只确认完整语句，`frame.rs` 负责字符图布局，`overview.rs`
//! 派生大图摘要，`preview.rs` 维护可信前缀与节流状态，`layout.rs` 共享几何缓存。

pub mod fence;
pub mod frame;
pub mod layout;
pub mod overview;
pub mod preview;
pub mod stream;

pub use fence::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity,
    MermaidMessageRef, is_mermaid_info,
};
