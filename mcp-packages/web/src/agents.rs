//! web 实例发布的 builtin Agent 定义资源面。
//!
//! `web-researcher` 的定义随 web 实例发布（`agent://builtin/web-researcher/agent.md`）：
//! 宿主 `McpAgentRegistry` 信任宿主绑定的 builtin 实例来源，因此该定义与 workspace
//! 实例发布的 builtin Agent 在目录面同形（scope = builtin、id 保持裸名），web 实例
//! 关闭时随实例一起不可发现。
//!
//! 边界：
//! - 只提供编译期嵌入的静态资产，不扫描目录、不读宿主配置，不做启用/关闭策略
//!   （§5.1「server 不持策略」）；
//! - frontmatter 解析复用 `peri_mcp_common::agent_definition::frontmatter`，本模块
//!   不解释能力、不因 frontmatter 声称提权；
//! - `_meta` 投影与 workspace 资源面同键：`io.peri/scope` / `io.peri/digest` /
//!   `io.peri/frontmatter`（宿主目录渲染据此推断 model tier / 写能力标签，正文
//!   仅在激活时经 `resources/read` 读取）。

use peri_acp_types::workspace_resources;
use peri_acp_types::workspace_resources::ResourceScope;
use peri_acp_types::workspace_resources::META_KEY_DIGEST;
use peri_acp_types::workspace_resources::META_KEY_FRONTMATTER;
use peri_acp_types::workspace_resources::META_KEY_SCOPE;
use peri_acp_types::workspace_resources::MIME_MARKDOWN;
use peri_mcp_common::agent_definition::frontmatter;
use rmcp::model::{MetaObject, Resource, ResourceContents};

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;

#[path = "agents/builtin_agent.rs"]
mod builtin_agent;
pub(crate) use builtin_agent::list_resources;
pub(crate) use builtin_agent::read;
pub(crate) use builtin_agent::AgentResourceError;
pub use builtin_agent::BuiltinAgent;
pub use builtin_agent::BUILTIN_AGENTS;
