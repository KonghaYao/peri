//! CurrentTurn — 见 `kit::acp_types`。
//!
//! S11 起类型定义迁移到 `kit::acp_types` 模块，本文件保留 re-export 以维持
//! legacy 路径（state_machine::current_turn::CurrentTurn）兼容。
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.3.

pub use crate::kit::acp_types::{CurrentTurn, ToolCardAccumulator};
