use std::fmt;
use std::fmt::{Debug, Formatter};
use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::oauth_credentials::{
    OAuthCredentialPort, OAuthCredentialRequest, OAuthCredentialResponse, OAuthCredentialValue,
    OAUTH_CREDENTIAL_METHOD,
};
use rmcp::model::ClientRequest;
use rmcp::model::CustomRequest;
use rmcp::model::ServerResult;
use rmcp::service::PeerRequestOptions;
use rmcp::transport::auth::StoredCredentials;
use rmcp::ServiceExt;

use crate::server::storage_error;
use crate::OAuthCredentialMcpServer;

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[path = "client/oauth_credential_client.rs"]
mod oauth_credential_client;
#[cfg(test)]
use oauth_credential_client::run_worker;
#[cfg(test)]
use oauth_credential_client::spawn_worker;
pub use oauth_credential_client::OAuthCredentialClient;
#[cfg(test)]
use oauth_credential_client::REQUEST_TIMEOUT;
