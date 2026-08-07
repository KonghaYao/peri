//! Compact 配置契约（自 peri-acp-types 迁入，本文件保留 re-export 保兼容）。
//!
//! `CompactConfig` 纯数据契约已归位 `peri-acp-types::compact`（配置来源为外部
//! 配置文件，跨层共享）；`CONTINUATION_HINT` 是 compact 摘要续接指令标记，
//! 作为单一事实源由 v2 自动 compact / `/compact` 命令 / TUI 识别层三方共享，
//! 保留在 Agent 层。

pub use peri_acp_types::compact::CompactConfig;

pub const CONTINUATION_HINT: &str =
    "[Context has been compacted. Continue working based on the summary above.]";

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
