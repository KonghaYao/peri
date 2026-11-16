//! System prompt construction.
//!
//! Assembles system prompt from middleware-owned sections and frozen overrides.

use std::sync::Arc;

use peri_acp_types::frozen::FrozenRuntimeEnv;
use peri_acp_types::model::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
use peri_acp_types::ports::AgentCatalogPort;
use peri_agent::middleware::{PromptSection, PromptSectionContent, PromptSectionZone};

#[cfg(test)]
thread_local! {
    /// 本线程的探测次数（测试证据：准入判定不得在远端 Workspace 会话探测宿主）。
    /// thread-local 使并发测试互不干扰（`#[tokio::test]` 默认单线程运行时，
    /// 被测准入路径与断言同线程）。
    static DETECT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) use prompt_template::detect_call_count;

#[cfg(test)]
pub(crate) use prompt_template::reset_detect_call_count;

pub use prompt_template::PromptRuntimeEnv;

pub use prompt_template::RUNTIME_ENV_UNAVAILABLE;

pub use prompt_template::PromptEnv;

pub use prompt_template::PromptTemplate;

pub(crate) use prompt_template::KNOWN_PLACEHOLDERS;

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;

pub(crate) mod section_validation;

#[cfg(test)]
#[path = "section_validation_test.rs"]
mod section_validation_tests;

#[path = "prompt_template.rs"]
mod prompt_template;
