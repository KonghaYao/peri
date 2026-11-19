//! Git attribution 中间件。
//!
//! 追踪 Write/Edit 工具修改的文件，累积贡献字符数。
//! Co-Authored-By 指令在 system prompt 构建时注入（`build_bare_agent`）。
//!
//! ## 钩子流程
//!
//! ```text
//! before_tool (Write/Edit) → 读取旧文件内容 → 存入 pending
//!   → [工具执行]         → after_tool       → 读取新文件内容 → track_change()
//!   → [审批阶段被拒绝]   → on_tool_rejected → 丢弃该调用的 pending（不计算贡献）
//! ```

mod model_email;
mod state;

use peri_agent::middleware::capabilities as hook_state;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
pub use model_email::attribution_email;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::react::ToolResult;
use peri_agent::error::AgentResult;
use peri_agent::middleware::r#trait::Middleware;
pub use state::AttributionState;

use crate::tool_search::core_tools::{TOOL_EDIT, TOOL_WRITE};

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "io_test.rs"]
mod io_tests;

#[cfg(test)]
#[path = "rejected_settlement_test.rs"]
mod rejected_settlement_tests;

mod git_attribution_middleware;
pub use git_attribution_middleware::GitAttributionMiddleware;
