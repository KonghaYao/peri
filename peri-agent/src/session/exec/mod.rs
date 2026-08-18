//! 执行装配与命令执行体（L5：自 `peri-acp/src/host/exec/` 迁入）。
//!
//! 本模块承载深绑 Agent 层执行类型的代码（§2 聚合根：归此层的职责以
//! session 生命周期为界）：
//! - [`executor`]：`run_session_loop` 会话编排（keepgoing 判定 / 空历史短路 /
//!   compact config / v2 MessageQueue + SessionInbox + AsyncRouter 接线 /
//!   bg_results 注入 / permission-mode 通知 / prediction facade；原 ACP
//!   `host/exec/executor.rs`，依赖反转后经端口与注入面接入）
//! - [`stage_builder`]：StageContext / AgentComponents 装配（原 `agent::builder`，
//!   装配上下文 `factory::AssemblyContext` 同层）
//! - [`executor_helpers`]：命令拦截 / 事件泵 / 结果收集 / v2 执行驱动
//!   （原 ACP `host/exec/executor_helpers.rs`，依赖反转后经端口与注入面接入）
//! - [`compact_pipeline`]：/compact 命令的 v2 执行体
//! - [`prompt_handle`]：执行句柄（`SessionHandle` 实现，runner 注入）
//! - [`events`]：命令事件发射辅助（EventSink 端口）
//!
//! 依赖方向（§0）：本模块只依赖 peri-acp-types / peri-model / peri-resources
//! 与 crate 内部；ACP 特有构造（LLM provider / 装配器 / 渲染 / 观测）经
//! 注入参数或端口接入，ACP 侧保留协议化薄壳与装配面。

pub mod compact_pipeline;
pub mod events;
pub mod executor;
pub mod executor_helpers;
pub mod prompt_handle;
pub mod stage_builder;
