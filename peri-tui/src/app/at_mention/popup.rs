//! @ 提及弹窗常量（kit 路径独立实现 @mention 组件）。
//!
//! Legacy `render_at_mention_popup` 已随 ui/ 删除一并退役。
//! kit 的 mention_popup.rs 是当前生产路径。保留 `MAX_VIEWPORT`
//! 因为 at_mention/mod.rs 的 `adjust_scroll` 仍引用此常量做视口钳位。

/// 弹窗最大显示行数
pub const MAX_VIEWPORT: usize = 10;
