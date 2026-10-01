use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use fs2::FileExt;
use peri_acp_types::configuration::{
    ConfigurationErrorKind, ConfigurationFailure, ConfigurationPaths, ConfigurationRequest,
    ConfigurationResponse, ConfigurationValue, CONFIGURATION_METHOD,
};
use rmcp::{
    model::{CustomRequest, CustomResult, Implementation, ServerCapabilities, ServerInfo},
    service::{RequestContext, RoleServer},
    ErrorData, ServerHandler,
};
use uuid::Uuid;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

struct WriteLockGuard {
    _process: MutexGuard<'static, ()>,
    _file: std::fs::File,
}

#[derive(Clone, Default)]
pub struct ConfigurationMcpServer {
    global_path: Arc<Mutex<Option<PathBuf>>>,
}

impl ConfigurationMcpServer {
    pub fn new() -> Self {
        Self::default()
    }

    fn execute(&self, request: ConfigurationRequest) -> io::Result<ConfigurationValue> {
        match request {
            ConfigurationRequest::ReadText { path } => {
                std::fs::read_to_string(path).map(ConfigurationValue::Text)
            }
            ConfigurationRequest::ReadEnvironment { name } => {
                if name.is_empty() || name.contains(['=', '\0']) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid configuration environment variable name",
                    ));
                }
                match std::env::var(name) {
                    Ok(value) => Ok(ConfigurationValue::Environment(Some(value))),
                    Err(std::env::VarError::NotPresent) => {
                        Ok(ConfigurationValue::Environment(None))
                    }
                    Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "configuration environment variable is not valid Unicode",
                    )),
                }
            }
            ConfigurationRequest::WriteTextAtomic { path, content } => {
                let _guard = write_lock(&path)?;
                write_atomic_locked(&path, &content)?;
                Ok(ConfigurationValue::Written)
            }
            ConfigurationRequest::WriteTextIfUnchanged {
                path,
                expected,
                content,
            } => {
                let _guard = write_lock(&path)?;
                let current = match std::fs::read(&path) {
                    Ok(current) => Some(current),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                if current.as_deref() != expected.as_deref().map(str::as_bytes) {
                    return Ok(ConfigurationValue::Bool(false));
                }
                write_atomic_locked(&path, &content)?;
                Ok(ConfigurationValue::Bool(true))
            }
            ConfigurationRequest::Exists { path } => {
                path.try_exists().map(ConfigurationValue::Bool)
            }
            ConfigurationRequest::SameFile { first, second } => {
                let same = if first == second {
                    true
                } else {
                    let first = canonical_path(&first)?;
                    let second = canonical_path(&second)?;
                    first.is_some() && first == second
                };
                Ok(ConfigurationValue::Bool(same))
            }
            ConfigurationRequest::Paths => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .or_else(dirs_next::home_dir);
                let global_settings = self
                    .global_path
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
                    .unwrap_or_else(|| {
                        home.clone()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join(".peri/settings.json")
                    });
                Ok(ConfigurationValue::Paths(ConfigurationPaths {
                    home,
                    cwd: std::env::current_dir().ok(),
                    global_settings,
                }))
            }
            ConfigurationRequest::SetGlobalPath { path } => {
                let path = path.map(|path| {
                    if path.is_relative() {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(&path))
                            .unwrap_or(path)
                    } else {
                        path
                    }
                });
                *self
                    .global_path
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = path;
                Ok(ConfigurationValue::Written)
            }
        }
    }
}

impl ServerHandler for ConfigurationMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::default()).with_server_info(Implementation::new(
            "peri-config-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        if request.method != CONFIGURATION_METHOD {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                request.method,
                None,
            ));
        }
        let request: ConfigurationRequest =
            serde_json::from_value(request.params.ok_or_else(|| {
                ErrorData::invalid_params("configuration operation required", None)
            })?)
            .map_err(|_| ErrorData::invalid_params("invalid configuration operation", None))?;
        let server = self.clone();
        let result = tokio::select! {
            biased;
            _ = context.ct.cancelled() => return Err(ErrorData::internal_error("configuration request cancelled", None)),
            result = tokio::task::spawn_blocking(move || server.execute(request)) => result
                .map_err(|_| ErrorData::internal_error("configuration operation failed", None))?,
        };
        let response: ConfigurationResponse = result.map_err(failure);
        let value = serde_json::to_value(response).map_err(|_| {
            ErrorData::internal_error("configuration response encoding failed", None)
        })?;
        Ok(CustomResult::new(value))
    }
}

fn write_lock(path: &Path) -> io::Result<WriteLockGuard> {
    let guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let canonical_parent = std::fs::canonicalize(parent)?;
    let filename = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration file path required",
        )
    })?;
    let lock_target = canonical_parent.join(filename);
    let mut hasher = DefaultHasher::new();
    lock_target.hash(&mut hasher);
    let lock_path = std::env::temp_dir().join(format!("peri-config-{:016x}.lock", hasher.finish()));
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    FileExt::lock_exclusive(&lock_file)?;
    Ok(WriteLockGuard {
        _process: guard,
        _file: lock_file,
    })
}

fn write_atomic_locked(path: &Path, content: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".peri-config-{}.tmp", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let outcome = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temporary, metadata.permissions())?;
        }
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

fn failure(error: io::Error) -> ConfigurationFailure {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => ConfigurationErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => ConfigurationErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists => ConfigurationErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput => ConfigurationErrorKind::InvalidInput,
        io::ErrorKind::InvalidData => ConfigurationErrorKind::InvalidData,
        io::ErrorKind::TimedOut => ConfigurationErrorKind::TimedOut,
        _ => ConfigurationErrorKind::Other,
    };
    let message = match &kind {
        ConfigurationErrorKind::NotFound => "configuration file not found",
        ConfigurationErrorKind::PermissionDenied => "configuration access denied",
        ConfigurationErrorKind::AlreadyExists => "configuration file already exists",
        ConfigurationErrorKind::InvalidInput => "invalid configuration input",
        ConfigurationErrorKind::InvalidData => "invalid configuration data",
        ConfigurationErrorKind::TimedOut => "configuration operation timed out",
        ConfigurationErrorKind::Other => "configuration I/O failed",
    }
    .into();
    ConfigurationFailure { kind, message }
}

fn canonical_path(path: &Path) -> io::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
