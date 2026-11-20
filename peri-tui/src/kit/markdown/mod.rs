//! 组件 Markdown 的应用内别名。
//!
//! 实现与公开 API 都在 `peri-tui_components::markdown`；这里仅保留 crate 内部路径，
//! 不再对外转发（应用调用点按原路径即可，避免无谓改名噪音）。

pub(crate) use peri_tui_components::markdown::*;
