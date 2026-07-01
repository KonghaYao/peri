//! 主题（re-export）——见 `kit::theme`。
//!
//! S11 起颜色常量定义迁移到 `kit::theme`，本文件保留 re-export 以维持
//! legacy 路径（ui::theme::ACCENT 等）兼容。

pub use crate::kit::theme::*;
