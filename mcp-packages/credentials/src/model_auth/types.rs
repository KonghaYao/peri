use serde::{Deserialize, Serialize};

pub type AuthResult<T> = Result<T, AuthError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    #[error("ChatGPT authentication was cancelled")]
    Cancelled,
    #[error("Invalid ChatGPT credential store or account reference")]
    InvalidReference,
    #[error("ChatGPT credential storage currently requires Unix owner-only file permissions")]
    UnsupportedPlatform,
    #[error("ChatGPT credentials are unavailable or have unsafe permissions")]
    Storage,
    #[error("ChatGPT authentication service is unavailable")]
    Unavailable,
    #[error("Sign in to ChatGPT for the selected account")]
    LoginRequired,
    #[error("ChatGPT identity or login session changed; use the matching login identity and create a new model session")]
    AccountChanged,
    #[error("ChatGPT sign-in callback validation failed")]
    InvalidCallback,
    #[error("ChatGPT sign-in was denied")]
    AccessDenied,
    #[error("ChatGPT ID token signature or claims are invalid")]
    InvalidIdToken,
    #[error("ChatGPT did not grant direct plan inference permission")]
    MissingScope,
    #[error("ChatGPT OAuth request failed: {code} {description}")]
    OAuth { code: String, description: String },
    #[error("ChatGPT authentication response is invalid")]
    InvalidResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatus {
    pub account_ref: String,
    pub subject: String,
    pub email: Option<String>,
    pub client_id: String,
    pub plan_enabled: bool,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutOutcome {
    pub local_cleared: bool,
    pub remote_revoked: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingLogin {
    pub(super) attempt_id: String,
    pub(super) authorization_url: String,
}

impl PendingLogin {
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }
}

impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLogin").finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    BeginLogin,
    FinishLogin {
        attempt_id: String,
    },
    Status,
    Logout,
    CancelRevocation,
    Credential {
        identity: ExpectedIdentity,
        binding: Option<String>,
    },
    Refresh {
        identity: ExpectedIdentity,
        binding: String,
        rejected_version: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ExpectedIdentity {
    pub subject: String,
    pub client_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(super) enum Response {
    Pending(PendingLogin),
    Status(Option<AccountStatus>),
    Logout(LogoutOutcome),
    RevocationCancelled,
    Credential(CredentialReply),
    Refreshed(bool),
}

#[derive(Serialize, Deserialize)]
pub(super) struct CredentialReply {
    pub bearer: String,
    pub version: String,
    pub binding: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: u64,
    pub scope: String,
    pub version: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredAccount {
    pub schema_version: u8,
    pub revision: String,
    pub host_id: String,
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub email: Option<String>,
    pub binding: Option<String>,
    pub tokens: Option<TokenSet>,
}

impl StoredAccount {
    pub fn new(host_id: String) -> Self {
        Self {
            schema_version: 1,
            revision: uuid::Uuid::new_v4().to_string(),
            host_id,
            client_id: None,
            subject: None,
            email: None,
            binding: None,
            tokens: None,
        }
    }

    pub fn status(&self, account_ref: &str) -> Option<AccountStatus> {
        let tokens = self.tokens.as_ref()?;
        Some(AccountStatus {
            account_ref: account_ref.to_owned(),
            subject: self.subject.clone()?,
            email: self.email.clone(),
            client_id: self.client_id.clone()?,
            plan_enabled: has_plan_scope(&tokens.scope),
            expires_at: tokens.expires_at,
        })
    }
}

pub(super) fn has_plan_scope(scope: &str) -> bool {
    ["resource.invoke", "chatgpt.tokens.use.direct"]
        .iter()
        .all(|required| scope.split_ascii_whitespace().any(|part| part == *required))
}
