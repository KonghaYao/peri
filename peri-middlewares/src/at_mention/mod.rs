mod parser;

use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use crate::workspace_io::{WorkspaceFileReader, WorkspaceMentionContent};
use async_trait::async_trait;
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::messages::BaseMessage;
use peri_agent::messages::ContentBlock;
use peri_agent::middleware::r#trait::Middleware;

#[cfg(test)]
#[path = "mod_test.rs"]
pub(crate) mod tests;

mod at_mention_middleware;

#[cfg(test)]
use at_mention_middleware::MAX_BATCH_MENTION_BYTES;
#[cfg(test)]
use at_mention_middleware::MAX_BATCH_MENTION_ITEMS;
#[cfg(test)]
use at_mention_middleware::MAX_MENTION_CONTENT_BYTES;

pub use at_mention_middleware::AtMentionMiddleware;

#[cfg(test)]
use at_mention_middleware::trim_mention_content;
