//! /bg 命令实现（3.0 批 2 归位 + L5 拆桥：实现在
//! `peri-agent::session::exec::bg`，装配注入面——SubAgent 发起深绑 Agent 层
//! 执行类型；本模块 re-export 保协议面调用兼容）。

pub use peri_agent::session::exec::bg::BgCommand;
