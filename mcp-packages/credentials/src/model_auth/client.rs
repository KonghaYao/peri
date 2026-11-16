use std::fmt;
use std::fmt::{Debug, Formatter};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use peri_model::{ModelError, ModelResult, ProtocolErrorKind, RequestAuth, RequestCredential};
use rmcp::model::CancelledNotificationParam;
use rmcp::model::ClientRequest;
use rmcp::model::CustomRequest;
use rmcp::model::ServerResult;
use rmcp::service::PeerRequestOptions;
use rmcp::ServiceExt;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::oauth::OAuth;
use super::server::AuthServer;
use super::server::Capability;
use super::store::Store;
use super::types::AccountStatus;
use super::types::AuthError;
use super::types::AuthResult;
use super::types::ExpectedIdentity;
use super::types::LogoutOutcome;
use super::types::PendingLogin;
use super::types::Request;
use super::types::Response;
use super::ISSUER;
use super::METHOD;

#[cfg(all(test, unix))]
#[path = "client_test.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "client_process_test.rs"]
mod process_tests;

#[path = "client/chat_gpt_auth_manager.rs"]
mod chat_gpt_auth_manager;
pub use chat_gpt_auth_manager::chatgpt_credential_ref;
pub use chat_gpt_auth_manager::ChatGptAuthManager;
