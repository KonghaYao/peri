//! Deployment-issued capabilities for Workspace task scopes. The model cannot issue these.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::Path,
    sync::Arc,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use rmcp::{model::RequestMetaObject, ErrorData as McpError};
use sha2::Sha256;

pub const TASK_SCOPE_META_KEY: &str = "peri/taskScope";
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct TaskScopeAuthority {
    inner: Arc<AuthorityKind>,
}

enum AuthorityKind {
    Local(Mutex<HashMap<String, TaskScopeCapability>>),
    SharedSecret([u8; 32]),
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

    /// For a separately deployed owner and Agent sharing a protected 32-byte key file.
    /// This never creates a key or logs its bytes. Both processes must load the same file.
    pub fn from_secret_file(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "task scope secret must be a regular 32-byte file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "task scope secret must not be accessible to group or other users",
                ));
            }
        }
        let mut key = [0u8; 32];
        file.read_exact(&mut key)?;
        Ok(Self {
            inner: Arc::new(AuthorityKind::SharedSecret(key)),
        })
    }

    /// Only trusted deployment code may call this. The resulting token is a bearer capability.
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
            AuthorityKind::SharedSecret(key) => {
                let encoded = URL_SAFE_NO_PAD.encode(session_id);
                let message = format!("v1.{encoded}");
                let mut mac = HmacSha256::new_from_slice(key).expect("fixed HMAC key");
                mac.update(message.as_bytes());
                format!(
                    "{message}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            }
        }
    }

    /// Sign only values obtained by the trusted host from a Store-issued execution lease.
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
            AuthorityKind::SharedSecret(key) => {
                let message = format!(
                    "v2.{}.{}.{}",
                    URL_SAFE_NO_PAD.encode(session_id),
                    epoch,
                    URL_SAFE_NO_PAD.encode(nonce)
                );
                let mut mac = HmacSha256::new_from_slice(key).expect("fixed HMAC key");
                mac.update(message.as_bytes());
                format!(
                    "{message}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn resolve(&self, meta: &RequestMetaObject) -> Result<String, McpError> {
        Ok(self.resolve_capability(meta)?.session_id)
    }

    pub(crate) fn resolve_capability(
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
            AuthorityKind::SharedSecret(key) => verify_signed(token, key),
        }
        .ok_or_else(|| McpError::invalid_params("invalid task scope capability", None))
    }
}

fn verify_signed(token: &str, key: &[u8; 32]) -> Option<TaskScopeCapability> {
    if token.len() > 512 {
        return None;
    }
    let (message, signature) = token.rsplit_once('.')?;
    let (encoded, execution) = if let Some(encoded) = message.strip_prefix("v1.") {
        (encoded, None)
    } else {
        let fields = message.strip_prefix("v2.")?;
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
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(message.as_bytes());
    mac.verify_slice(&signature).ok()?;
    Some(TaskScopeCapability {
        session_id: session,
        execution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_scope_survives_authority_recreation_and_rejects_tampering() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scope-secret");
        std::fs::write(&path, [7u8; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let issuer = TaskScopeAuthority::from_secret_file(&path).unwrap();
        let verifier = TaskScopeAuthority::from_secret_file(&path).unwrap();
        let token = issuer.issue("session-a");
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(TASK_SCOPE_META_KEY.into(), token.into());
        assert_eq!(verifier.resolve(&meta).unwrap(), "session-a");
        meta.0 .0.insert(
            TASK_SCOPE_META_KEY.into(),
            issuer.issue("session-b").replace("v1.", "v2.").into(),
        );
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
        meta.0 .0.insert(
            TASK_SCOPE_META_KEY.into(),
            execution.replace(".3.", ".4.").into(),
        );
        assert!(verifier.resolve_capability(&meta).is_err());
    }

    #[test]
    fn secret_file_rejects_unsafe_permissions_and_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scope-secret");
        std::fs::write(&path, [1u8; 31]).unwrap();
        assert!(TaskScopeAuthority::from_secret_file(&path).is_err());
        std::fs::write(&path, [1u8; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(TaskScopeAuthority::from_secret_file(&path).is_err());
        }
    }
}
