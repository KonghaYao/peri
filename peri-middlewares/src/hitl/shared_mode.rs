//! 权限模式（3.0 批 2 波 1：协议类型归契约层，本模块保留 re-export 保兼容）。
//!
//! 定义见 [`peri_acp_types::permission`]（`PermissionMode` / `SharedPermissionMode`）。

pub use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};

#[cfg(test)]
#[path = "shared_mode_test.rs"]
mod tests;
