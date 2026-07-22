use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

const TOKEN_FILE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct OAuthTokenFile {
    version: u32,
    tokens: HashMap<String, StoredCredentials>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthStoreError {
    #[error("Token 文件读取失败: {path}: {detail}")]
    ReadFailed { path: PathBuf, detail: String },
    #[error("Token 文件写入失败: {path}: {detail}")]
    WriteFailed { path: PathBuf, detail: String },
    #[error("Token 文件格式无效: {reason}")]
    InvalidFormat { reason: String },
}

pub struct FileCredentialStore {
    path: PathBuf,
    mutex: Mutex<()>,
}

impl Default for FileCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileCredentialStore {
    pub fn new() -> Self {
        let path = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".peri")
            .join("oauth_tokens.json");
        Self {
            path,
            mutex: Mutex::new(()),
        }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            mutex: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_file(&self) -> Result<(), AuthStoreError> {
        if !self.path.exists() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AuthStoreError::WriteFailed {
                    path: parent.to_path_buf(),
                    detail: e.to_string(),
                })?;
            }
            let initial_content = serde_json::to_string_pretty(&OAuthTokenFile {
                version: TOKEN_FILE_VERSION,
                tokens: HashMap::new(),
            })
            .map_err(|e| AuthStoreError::WriteFailed {
                path: self.path.clone(),
                detail: e.to_string(),
            })?;
            std::fs::write(&self.path, initial_content).map_err(|e| {
                AuthStoreError::WriteFailed {
                    path: self.path.clone(),
                    detail: e.to_string(),
                }
            })?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| AuthStoreError::WriteFailed {
                    path: self.path.clone(),
                    detail: e.to_string(),
                },
            )?;
        }
        Ok(())
    }

    fn read_file(&self) -> Result<OAuthTokenFile, AuthStoreError> {
        self.ensure_file()?;
        let content =
            std::fs::read_to_string(&self.path).map_err(|e| AuthStoreError::ReadFailed {
                path: self.path.clone(),
                detail: e.to_string(),
            })?;
        let file: OAuthTokenFile =
            serde_json::from_str(&content).map_err(|e| AuthStoreError::InvalidFormat {
                reason: format!("JSON 解析失败: {}", e),
            })?;
        if file.version != TOKEN_FILE_VERSION {
            return Err(AuthStoreError::InvalidFormat {
                reason: format!(
                    "不支持的版本号: {}，期望: {}",
                    file.version, TOKEN_FILE_VERSION
                ),
            });
        }
        Ok(file)
    }

    fn write_file(&self, file: &OAuthTokenFile) -> Result<(), AuthStoreError> {
        self.ensure_file()?;
        let content =
            serde_json::to_string_pretty(file).map_err(|e| AuthStoreError::WriteFailed {
                path: self.path.clone(),
                detail: e.to_string(),
            })?;
        std::fs::write(&self.path, content).map_err(|e| AuthStoreError::WriteFailed {
            path: self.path.clone(),
            detail: e.to_string(),
        })?;
        debug!("Token 文件已写入: {}", self.path.display());
        Ok(())
    }

    pub async fn load_server(
        &self,
        server_name: &str,
    ) -> Result<Option<StoredCredentials>, AuthStoreError> {
        let _lock = self.mutex.lock().await;
        let file = self.read_file()?;
        Ok(file.tokens.get(server_name).cloned())
    }

    pub async fn save_server(
        &self,
        server_name: &str,
        credentials: StoredCredentials,
    ) -> Result<(), AuthStoreError> {
        let _lock = self.mutex.lock().await;
        let mut file = self.read_file()?;
        file.tokens.insert(server_name.to_string(), credentials);
        self.write_file(&file)
    }

    pub async fn clear_server(&self, server_name: &str) -> Result<(), AuthStoreError> {
        let _lock = self.mutex.lock().await;
        let mut file = self.read_file()?;
        file.tokens.remove(server_name);
        self.write_file(&file)
    }

    pub async fn clear_all(&self) -> Result<(), AuthStoreError> {
        let _lock = self.mutex.lock().await;
        self.write_file(&OAuthTokenFile {
            version: TOKEN_FILE_VERSION,
            tokens: HashMap::new(),
        })
    }

    pub async fn list_servers(&self) -> Result<Vec<String>, AuthStoreError> {
        let _lock = self.mutex.lock().await;
        Ok(self.read_file()?.tokens.keys().cloned().collect())
    }
}

pub struct PerServerCredentialStore {
    inner: Arc<FileCredentialStore>,
    server_name: String,
}

impl PerServerCredentialStore {
    pub fn new(inner: Arc<FileCredentialStore>, server_name: String) -> Self {
        Self { inner, server_name }
    }
}

#[async_trait]
impl CredentialStore for PerServerCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.inner
            .load_server(&self.server_name)
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.inner
            .save_server(&self.server_name, credentials)
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.inner
            .clear_server(&self.server_name)
            .await
            .map_err(|e| AuthError::InternalError(e.to_string()))
    }
}

#[cfg(test)]
#[path = "auth_store_test.rs"]
mod tests;
