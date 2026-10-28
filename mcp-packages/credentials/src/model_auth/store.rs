use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use super::types::{AuthError, AuthResult, StoredAccount};

const MAX_STORE_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
pub(super) struct Store {
    root: PathBuf,
    account: String,
}

/// A native advisory lock covers read, network refresh and atomic rotation save.
/// All processes using this capability acquire the same account lock file.
pub(super) struct Lease {
    _lock: Arc<File>,
    path: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf, account: String) -> AuthResult<Self> {
        if !cfg!(unix) {
            return Err(AuthError::UnsupportedPlatform);
        }
        if !root.is_absolute() || !peri_acp_types::model::valid_account_reference(&account) {
            return Err(AuthError::InvalidReference);
        }
        Ok(Self { root, account })
    }

    pub async fn lease(&self, cancellation: &CancellationToken) -> AuthResult<Lease> {
        self.lock_named(&format!("account-{}", self.account), cancellation)
            .await
    }

    pub async fn host_id(&self, cancellation: &CancellationToken) -> AuthResult<String> {
        let lease = self.lock_named(".host", cancellation).await?;
        let path = lease.path.clone();
        let lock = lease._lock.clone();
        blocking(move || {
            let _lock = lock;
            if let Some(value) = read_json::<String>(&path)? {
                let id = value
                    .strip_prefix("urn:uuid:")
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                if id.is_none_or(|id| id.get_version_num() != 4) {
                    return Err(AuthError::Storage);
                }
                return Ok(value);
            }
            let host_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
            atomic_json(&path, &host_id)?;
            Ok(host_id)
        })
        .await
    }

    async fn lock_named(&self, name: &str, cancellation: &CancellationToken) -> AuthResult<Lease> {
        let root = self.root.clone();
        let name = name.to_owned();
        let (file, path) = blocking(move || {
            create_private_directory(&root)?;
            let path = root.join(format!("{name}.json"));
            let lock_path = root.join(format!("{name}.lock"));
            reject_symlink(&lock_path)?;
            let mut options = OpenOptions::new();
            options.create(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(lock_path).map_err(io_error)?;
            check_private(&file.metadata().map_err(io_error)?)?;
            Ok((Arc::new(file), path))
        })
        .await?;
        let deadline = peri_time::monotonic_now() + Duration::from_secs(120);
        loop {
            if cancellation.is_cancelled() {
                return Err(AuthError::Cancelled);
            }
            let candidate = file.clone();
            let acquired = blocking(move || match candidate.try_lock() {
                Ok(()) => Ok(true),
                Err(std::fs::TryLockError::WouldBlock) => Ok(false),
                Err(std::fs::TryLockError::Error(error)) => Err(io_error(error)),
            })
            .await?;
            if acquired {
                return Ok(Lease { _lock: file, path });
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AuthError::Cancelled),
                _ = peri_time::sleep_until(deadline) => return Err(AuthError::Unavailable),
                _ = peri_time::sleep(Duration::from_millis(25)) => {}
            }
        }
    }
}

impl Lease {
    pub async fn read(&self) -> AuthResult<Option<StoredAccount>> {
        let path = self.path.clone();
        let lock = self._lock.clone();
        blocking(move || {
            let _lock = lock;
            read_json(&path)
        })
        .await
    }

    pub async fn save(&self, record: StoredAccount) -> AuthResult<()> {
        let path = self.path.clone();
        let lock = self._lock.clone();
        blocking(move || {
            let _lock = lock;
            atomic_json(&path, &record)
        })
        .await
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> AuthResult<T> + Send + 'static,
) -> AuthResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        tracing::error!(%error, "ChatGPT credential storage task failed");
        AuthError::Storage
    })?
}

fn io_error(error: std::io::Error) -> AuthError {
    tracing::warn!(%error, "ChatGPT credential storage failed");
    AuthError::Storage
}

fn create_private_directory(path: &Path) -> AuthResult<()> {
    reject_symlink(path)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(io_error)?;
    let metadata = std::fs::metadata(path).map_err(io_error)?;
    if !metadata.is_dir() {
        return Err(AuthError::Storage);
    }
    check_private(&metadata)
}

fn reject_symlink(path: &Path) -> AuthResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AuthError::Storage),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn check_private(metadata: &std::fs::Metadata) -> AuthResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AuthError::Storage);
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> AuthResult<Option<T>> {
    reject_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let metadata = file.metadata().map_err(io_error)?;
    check_private(&metadata)?;
    if !metadata.is_file() || metadata.len() > MAX_STORE_BYTES {
        return Err(AuthError::Storage);
    }
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        tracing::warn!(line = error.line(), column = error.column(), category = ?error.classify(), "Invalid ChatGPT credential record");
        AuthError::Storage
    })
}

fn atomic_json(path: &Path, value: &impl serde::Serialize) -> AuthResult<()> {
    let parent = path.parent().ok_or(AuthError::Storage)?;
    reject_symlink(path)?;
    let bytes = serde_json::to_vec(value).map_err(|_| AuthError::Storage)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(AuthError::Storage);
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    temp.write_all(&bytes).map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    temp.persist(path).map_err(|error| io_error(error.error))?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "store_test.rs"]
mod tests;
