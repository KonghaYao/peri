//! /bg 命令实现（3.0 批 2 归位：实现在 `crate::host::exec::bg`，
//! 装配注入面——SubAgent 发起深绑 Agent 层执行类型；本模块 re-export 保兼容）。

pub use crate::host::exec::bg::BgCommand;
