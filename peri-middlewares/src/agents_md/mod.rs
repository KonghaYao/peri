//! AgentsMdMiddleware — 项目指令（AGENTS.md / CLAUDE.md / CLAUDE.local.md）
//! 的**纯贡献 adapter**（W5，plan §6.1/§6.3）。
//!
//! 迁移事实（E14/E15 → plan §6.3「删除全部读盘/搜索/import 行为」）：
//!
//! - **不再读盘**：正文由会话创建期的内容准入（P4）从 builtin `workspace`
//!   实例的 `peri-instruction://workspace/{main|local}` 资源读取（provider 负责
//!   候选优先级、`@import` 深度 3 展开与环防护、越界拒绝），随冻结数据注入
//!   （`FrozenInstructions` → `with_frozen_parts`）；
//! - 本中间件只保留「贡献字符串的持有与注入」：`prompt_contribution()` 返回
//!   main + 空行 + local 的合成文本；`before_agent` 是无副作用空 hook；
//! - 找不到内容（无冻结正文 / 指令面被关闭 / 未装配）⇒ 不贡献，
//!   **不回落磁盘**（X4/J5：任何组件不得回落）；
//! - `excludes` 语义已随候选选择整体归 provider 输入
//!   （`WorkspaceResourcesInput::instruction_excludes`），本中间件不再持有它。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use peri_agent::error::AgentResult;
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::middleware::r#trait::Middleware;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

mod agents_md_middleware;
pub use agents_md_middleware::AgentsMdMiddleware;
