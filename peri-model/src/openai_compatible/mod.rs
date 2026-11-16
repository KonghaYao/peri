//! OpenAI-compatible Chat Completions provider。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。

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
use crate::transport::HttpTransport;
use crate::transport::ReqwestTransport;
use crate::ModelCapabilities;
use crate::ModelError;
use crate::ModelRequest;
use crate::ModelResult;
use crate::ModelRuntimeConfig;
use crate::ModelStream;
use crate::PreparedModelRequest;

use request::BuiltOpenAiRequest;

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod diagnostic_test;

mod open_ai_model;

pub use open_ai_model::OpenAiConfig;
pub use open_ai_model::OpenAiModel;
