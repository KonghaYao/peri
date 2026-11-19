//! Anthropic Messages API adapter。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。

mod cache;
mod request;
mod response;
mod stream;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::runtime::stream::runtime_http_sse_stream;
use crate::transport::HttpRequest;
#[cfg(test)]
use crate::transport::HttpTransport;
use crate::transport::ModelHttp;
use crate::ModelCapabilities;
use crate::ModelError;
use crate::ModelRequest;
use crate::ModelResult;
use crate::ModelRuntimeConfig;
use crate::ModelStream;
use crate::PreparedModelRequest;

use request::BuiltAnthropicRequest;

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;

#[cfg(test)]
#[path = "config_test.rs"]
mod config_test;
#[cfg(test)]
#[path = "stream_failure_test.rs"]
mod stream_failure_test;
#[cfg(test)]
#[path = "stream_lifecycle_test.rs"]
mod stream_lifecycle_test;

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod diagnostic_test;

mod anthropic_model;

pub use anthropic_model::AnthropicConfig;
pub use anthropic_model::AnthropicModel;
