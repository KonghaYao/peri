//! Compact 配置 + 续接指令常量
//!
//! [v2] v1 compact/ 主体（full / micro / re_inject / invariant）已物理删除，
//! 实现迁移到 `crate::agent::compact_v2`。此模块仅保留：
//! - `config::CompactConfig`：v2 stages/compact.rs 消费的配置类型
//! - `CONTINUATION_HINT`：compact 摘要消息的续接指令标记，三方共享
//!   （v2 compact_v2.rs / `/compact` 命令 invariant.rs / TUI 识别层 build.rs）

pub mod config;

pub use config::CompactConfig;

/// Compact 摘要 Human 消息的续接指令标记。
///
/// 作为单一事实源，由三条路径共享：
/// - v2 自动 compact（`crate::agent::compact_v2::full_compact_inner`）
/// - `/compact` 命令路径（`peri-acp/src/session/command/compact/invariant.rs`）
/// - TUI 识别层（`peri-tui/src/ui/message_view/build.rs::COMPACT_HINT`）
///
/// 修改时必须同步三方，否则 compact 输出不被 TUI 折叠显示。
pub const CONTINUATION_HINT: &str =
    "[Context has been compacted. Continue working based on the summary above.]";
