//! Compact Pipeline 协议化薄壳（3.0 批 2 归位）。
//!
//! `/compact` 命令的 v2 执行体（`MessageTranscript` / `compact_v2::run_compact`
//! 深绑 Agent 层执行类型）已归位到 `crate::host::exec::compact_pipeline`
//! （过渡宿主：全路径引用，豁免至 L5——见
//! `spec/issues/2026-08-05-3.0-acp-events-session-batch2.md`；随 executor 拆分
//! 物理迁入 peri-agent）；本模块 re-export 保兼容，协议面不直接触碰
//! Agent 层类型。
//!
//! 阶段顺序（见 `host::exec::compact_pipeline` 模块注释）：
//!   validate_inputs → resolve_auxiliary_model → (emit_started)
//!   → run_v2_compact_with_cancel → assemble_compact_messages
//!   → (emit_completed)

pub use crate::host::exec::compact_pipeline::execute_compact;
