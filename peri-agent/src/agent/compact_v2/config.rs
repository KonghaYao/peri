//! Compact 配置契约（自 peri-acp-types 迁入，本文件保留 re-export 保兼容）。
//!
//! `CompactConfig` 与 `CONTINUATION_HINT` 契约已归位 `peri-acp-types::compact`
//!（配置来源为外部配置文件，跨层共享）；本模块保留 re-export，供既有 Agent
//! 调用方兼容访问。

pub use peri_acp_types::compact::{CompactConfig, CONTINUATION_HINT};

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
