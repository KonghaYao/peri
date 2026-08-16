//! Compact 事件发射桥（L5 拆桥：发射辅助已迁入
//! `peri-agent::session::exec::events`，本模块 re-export 保协议面引用兼容）。
//!
//! [桥期] ACP 内部当前无调用方（compact 执行体在 peri-agent 侧自行发射），
//! 本桥保留 re-export 供协议面后续命令面回引；pub(crate) 模块内 pub use
//! 无外部可见性，unused_imports 豁免。
//!
//! [TRAP] CompactCompleted 事件被 TUI 通过 StateSnapshot + 流式事件维护状态消费
//! （MessageAdded 被 TUI 丢弃）。事件字段 messages 与 CommandResult.messages 共享
//! new_messages.clone() —— 必须保持引用一致性。
//! （详见 CLAUDE.md TUI 事件映射章节、spec/global/domains/compact.md）
//!
//! Phase 5 Step 4：CompactError 变体与发射辅助已删除（错误反馈收敛到
//! CommandFeedback）；CompactCompleted 收敛为 { summary, messages, trigger }
//! 重建信号三字段。
#![allow(unused_imports)]

pub use peri_agent::session::exec::events::{
    emit_compact_completed, emit_compact_started, COMPACT_CONTEXT_WINDOW,
};
