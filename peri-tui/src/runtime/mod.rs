//! TUI runtime -- effect + event channel scaffolding.
//!
//! Legacy main loop / collectors / apply_context 已于 S13b 整片删除：
//! kit 路径（`crate::kit::entry::run_kit_fullscreen`）成为唯一启动入口。
//! 保留 `effect`（command + state_machine transitions 依赖）和
//! `event_channel`（state_machine::event::From<TuiEvent> 依赖）。

pub mod effect;
pub mod event_channel;
