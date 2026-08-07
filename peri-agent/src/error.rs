//! Agent 层边界错误（事实源 `peri-acp-types::error`，本模块 re-export 保兼容）。
//!
//! §9 错误模型：仅三类必须类型化——终止类（cancel/interrupt，防 `?` 误报失败）、
//! 可重试类、协议错误；TurnError 语义保留 Agent 层。

pub use peri_acp_types::error::{AgentError, AgentResult};
