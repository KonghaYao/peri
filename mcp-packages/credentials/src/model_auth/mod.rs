//! Host-only ChatGPT authentication capability. Every public operation crosses
//! the credentials MCP channel; neither tokens nor auth controls are model tools.
mod callback;
mod client;
mod jwt;
mod oauth;
mod server;
mod store;
mod types;

pub use client::{chatgpt_credential_ref, ChatGptAuthManager};
pub use types::{AccountStatus, AuthError, AuthResult, LogoutOutcome, PendingLogin};

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const METHOD: &str = "peri/chatgpt_auth";
