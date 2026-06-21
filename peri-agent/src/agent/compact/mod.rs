pub mod config;
pub mod full;
pub mod invariant;
pub mod micro;
pub mod re_inject;

pub use config::CompactConfig;
pub use full::{full_compact, FullCompactResult};
pub use micro::micro_compact_enhanced;
pub use re_inject::{extract_file_info, extract_skill_names, re_inject, ReInjectResult};

/// Compact 摘要 Human 消息的续接指令标记。
///
/// 作为单一事实源，由三条路径共享：
/// - `/compact` 命令路径（`peri-acp/src/session/command/compact/invariant.rs`）
/// - 自动 compact 路径（`peri-middlewares/src/compact_middleware.rs`）
/// - TUI 识别层（`peri-tui/src/ui/message_view/build.rs::COMPACT_HINT`）
///
/// 修改时必须同步三方，否则 `/compact` 命令的输出不被 TUI 折叠显示。
pub const CONTINUATION_HINT: &str =
    "[Context has been compacted. Continue working based on the summary above.]";
