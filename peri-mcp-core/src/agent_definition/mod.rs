//! Claude Code Agent 文件解析器
//!
//! 解析 Claude Code 格式的 agent 定义文件（Markdown with YAML frontmatter）
//!
//! 文件格式示例：
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Reviews code for quality and best practices
//! tools: Read, Glob, Grep
//! model: sonnet
//! ---
//!
//! You are a code reviewer...
//! ```
//!
//! [`frontmatter`] 提供与解析器同源的原文投影（YAML → JSON）：workspace 的
//! 本地资源与 web 实例的 builtin agent 资源共用它构造 `_meta`，避免两份
//! frontmatter 规则漂移。

use serde::de::Error;
use serde::Deserialize;

pub mod frontmatter;

#[cfg(test)]
#[path = "claude_agent_parser_test.rs"]
mod tests;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and retains the descriptive name of the multi-type implementation leaf."
)]
mod agent_definition;
pub use agent_definition::parse_agent_file;
#[cfg(test)]
use agent_definition::parse_agent_file_inner;
pub use agent_definition::ClaudeAgent;
pub use agent_definition::ClaudeAgentFrontmatter;
pub use agent_definition::ToolsValue;
