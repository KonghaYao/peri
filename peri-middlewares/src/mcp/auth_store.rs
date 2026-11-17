use async_trait::async_trait;
pub use peri_mcp_credentials::OAuthCredentialClient;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fmt::{Debug, Formatter};

use super::config::OAuthConfig;

#[cfg(test)]
#[path = "auth_store_test.rs"]
mod tests;

#[path = "auth_store/per_server_credential_store.rs"]
mod per_server_credential_store;
pub(crate) use per_server_credential_store::static_credential_key;
pub use per_server_credential_store::PerServerCredentialStore;
