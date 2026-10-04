//! Deployment-issued capabilities for Workspace task scopes. The model cannot issue these.

use std::{collections::HashMap, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use parking_lot::Mutex;
use rmcp::{model::RequestMetaObject, ErrorData as McpError};

pub const TASK_SCOPE_META_KEY: &str = "peri/taskScope";

#[derive(Clone)]
pub struct TaskScopeAuthority {
    inner: Arc<AuthorityKind>,
}

enum AuthorityKind {
    Local(Mutex<HashMap<String, TaskScopeCapability>>),
    TrustedConnection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionGeneration {
    pub epoch: i64,
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskScopeCapability {
    pub session_id: String,
    pub execution: Option<ExecutionGeneration>,
}

impl Default for TaskScopeAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskScopeAuthority {
    /// For a builtin owner and its trusted host in the same process.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AuthorityKind::Local(Mutex::new(HashMap::new()))),
        }
    }

    /// For a separately deployed owner reached through a deployment-trusted MCP connection.
    /// The connection, not a second shared key, establishes the caller boundary.
    pub fn trusted_connection() -> Self {
        Self {
            inner: Arc::new(AuthorityKind::TrustedConnection),
        }
    }

    /// The host supplies the session identity; transport admission protects this metadata.
    pub fn issue(&self, session_id: &str) -> String {
        match self.inner.as_ref() {
            AuthorityKind::Local(tokens) => {
                let token = uuid::Uuid::new_v4().to_string();
                tokens.lock().insert(
                    token.clone(),
                    TaskScopeCapability {
                        session_id: session_id.into(),
                        execution: None,
                    },
                );
                token
            }
            AuthorityKind::TrustedConnection => {
                format!("v1.{}", URL_SAFE_NO_PAD.encode(session_id))
            }
        }
    }

    /// Encode only values obtained by the trusted host from a Store-issued execution lease.
    pub fn issue_execution(&self, session_id: &str, epoch: i64, nonce: &str) -> String {
        assert!(
            epoch > 0 && !nonce.is_empty(),
            "invalid Store execution generation"
        );
        let capability = TaskScopeCapability {
            session_id: session_id.into(),
            execution: Some(ExecutionGeneration {
                epoch,
                nonce: nonce.into(),
            }),
        };
        match self.inner.as_ref() {
            AuthorityKind::Local(tokens) => {
                let token = uuid::Uuid::new_v4().to_string();
                tokens.lock().insert(token.clone(), capability);
                token
            }
            AuthorityKind::TrustedConnection => {
                format!(
                    "v2.{}.{}.{}",
                    URL_SAFE_NO_PAD.encode(session_id),
                    epoch,
                    URL_SAFE_NO_PAD.encode(nonce)
                )
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn resolve(&self, meta: &RequestMetaObject) -> Result<String, McpError> {
        Ok(self.resolve_capability(meta)?.session_id)
    }

    /// Verify a request capability before a Workspace handler uses its session and generation.
    pub fn resolve_capability(
        &self,
        meta: &RequestMetaObject,
    ) -> Result<TaskScopeCapability, McpError> {
        let token = meta
            .0
             .0
            .get(TASK_SCOPE_META_KEY)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| McpError::invalid_params("missing task scope capability", None))?;
        match self.inner.as_ref() {
            AuthorityKind::Local(tokens) => tokens.lock().get(token).cloned(),
            AuthorityKind::TrustedConnection => decode_scope(token),
        }
        .ok_or_else(|| McpError::invalid_params("invalid task scope capability", None))
    }
}

fn decode_scope(token: &str) -> Option<TaskScopeCapability> {
    if token.len() > 512 {
        return None;
    }
    let (encoded, execution) = if let Some(encoded) = token.strip_prefix("v1.") {
        (encoded, None)
    } else {
        let fields = token.strip_prefix("v2.")?;
        let mut parts = fields.split('.');
        let encoded = parts.next()?;
        let epoch = parts.next()?.parse::<i64>().ok()?;
        let nonce = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
        if parts.next().is_some() || epoch <= 0 || nonce.is_empty() {
            return None;
        }
        let nonce = String::from_utf8(nonce).ok()?;
        (encoded, Some(ExecutionGeneration { epoch, nonce }))
    };
    let session = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let session = String::from_utf8(session).ok()?;
    if session.is_empty() {
        return None;
    }
    Some(TaskScopeCapability {
        session_id: session,
        execution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_connection_scope_survives_authority_recreation() {
        let issuer = TaskScopeAuthority::trusted_connection();
        let verifier = TaskScopeAuthority::trusted_connection();
        let token = issuer.issue("session-a");
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(TASK_SCOPE_META_KEY.into(), token.into());
        assert_eq!(verifier.resolve(&meta).unwrap(), "session-a");
        meta.0
             .0
            .insert(TASK_SCOPE_META_KEY.into(), "invalid-scope".into());
        assert!(verifier.resolve(&meta).is_err());
        let execution = issuer.issue_execution("session-a", 3, "claim-nonce");
        meta.0
             .0
            .insert(TASK_SCOPE_META_KEY.into(), execution.clone().into());
        assert_eq!(
            verifier.resolve_capability(&meta).unwrap().execution,
            Some(ExecutionGeneration {
                epoch: 3,
                nonce: "claim-nonce".into()
            })
        );
        assert!(verifier.resolve_capability(&meta).is_ok());
    }
}
