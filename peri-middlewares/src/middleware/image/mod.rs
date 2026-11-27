mod compressor;
mod reader;
#[cfg(test)]
mod test_support;

use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::error::AgentResult;
use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent};
use peri_agent::middleware::r#trait::Middleware;
use regex::Regex;

pub use compressor::{CompressorPipeline, ImageCompressor};

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

mod image_middleware;
pub use image_middleware::ImageMiddleware;
