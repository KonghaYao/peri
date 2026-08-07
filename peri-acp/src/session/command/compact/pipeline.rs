//! Compact Pipeline 协议化薄壳（3.0 批 2 归位 + L5 拆桥）。
//!
//! `/compact` 命令的 v2 执行体（`MessageTranscript` / `compact_v2::run_compact`
//! 深绑 Agent 层执行类型）已随 L5 物理迁入
//! `peri-agent::session::exec::compact_pipeline`；本模块 re-export 保协议面
//! 调用兼容（阶段顺序见 peri-agent 侧模块注释）：
//!   validate_inputs → resolve_auxiliary_model → (emit_started)
//!   → run_v2_compact_with_cancel → assemble_compact_messages
//!   → (emit_completed)

pub use peri_agent::session::exec::compact_pipeline::execute_compact;
