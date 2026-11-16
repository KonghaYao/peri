use serde_json::Value;
use std::fmt;
use std::fmt::{Debug, Formatter};
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelResult, ModelStream, ProtocolErrorKind};

#[path = "prepared/prepared_model_call.rs"]
mod prepared_model_call;
pub(crate) use prepared_model_call::checkpoint;
pub use prepared_model_call::PreparedModelCall;
