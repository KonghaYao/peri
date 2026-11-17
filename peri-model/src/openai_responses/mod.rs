//! OpenAI Responses API adapter shared by API keys and ChatGPT plan usage.
//!
//! Wire semantics are based on the public Responses documentation and OpenAI
//! Codex (Apache-2.0), revision 970111f6cb62cc02fe36a82ea7d71b37a2889c97.
//! Peri owns transport, retry, cancellation and tool execution.

mod replay;
mod request;
mod request_content;
mod response;
mod stream;
mod stream_item;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
use crate::PreparedModelCall;
use crate::PreparedModelRequest;
use crate::ProtocolErrorKind;
use crate::RequestAuth;
use crate::RequestCredential;

use request::BuiltResponsesRequest;

#[cfg(test)]
#[path = "request_test.rs"]
mod request_test;

#[cfg(test)]
#[path = "stream_test.rs"]
mod stream_test;

#[cfg(test)]
#[path = "stream_incomplete_test.rs"]
mod stream_incomplete_test;

mod responses_model;
pub use responses_model::ResponsesConfig;
pub use responses_model::ResponsesModel;
pub use responses_model::ResponsesProfile;
pub(super) use responses_model::PROVIDER_NAME;
pub(super) use responses_model::TOOL_NAMESPACE;
