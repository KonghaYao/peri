use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use peri_acp_types::oauth_credentials;
use peri_acp_types::oauth_credentials::OAuthCredentialError;
use peri_acp_types::oauth_credentials::OAuthCredentialPort;
use peri_acp_types::oauth_credentials::OAuthCredentialRequest;
use peri_acp_types::oauth_credentials::OAuthCredentialResponse;
use peri_acp_types::oauth_credentials::OAuthCredentialValue;
use peri_acp_types::oauth_credentials::OAUTH_CREDENTIAL_METHOD;
use rmcp::model::CustomRequest;
use rmcp::model::CustomResult;
use rmcp::model::Implementation;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData;
use rmcp::ServerHandler;

#[path = "server/oauth_credential_mcp_server.rs"]
mod oauth_credential_mcp_server;
pub(crate) use oauth_credential_mcp_server::storage_error;
pub use oauth_credential_mcp_server::OAuthCredentialMcpServer;
