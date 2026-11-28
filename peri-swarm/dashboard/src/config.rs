//! Credential configuration for the dashboard.
//!
//! The file lives at `~/.config/peri-swarm-dashboard/config.toml`, the directory
//! is `0700`, and the file is written atomically with mode `0600`. The API key
//! and the OpenObserve password are never part of `Debug` output or error
//! messages.
//!
//! The optional `[observability]` section holds the OpenObserve connection used
//! by the read-only SQL pages. OpenObserve OSS v1.2.0 has no viewer role, so the
//! dashboard uses a dedicated admin account and the client only sends `SELECT`
//! statements. That client-side check is **not** a security boundary: the
//! account itself can write. Protection depends on the private network, the
//! dedicated account, and keeping/rotating the `0600` credential file.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Directory under the user's config root.
pub const CONFIG_DIR_NAME: &str = "peri-swarm-dashboard";
/// File name inside [`CONFIG_DIR_NAME`].
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Default gateway origin offered on first run.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";
/// Default OpenObserve origin offered by the observability form.
pub const DEFAULT_OBSERVABILITY_BASE_URL: &str = "http://127.0.0.1:5080";
/// Default OpenObserve organization offered by the observability form.
pub const DEFAULT_OBSERVABILITY_ORG: &str = "default";

const REDACTED_KEY: &str = "<redacted>";

/// Why a base URL cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BaseUrlError {
    /// The URL was empty after trimming.
    #[error("base URL must not be empty")]
    Empty,
    /// The value is not an absolute URL with a host.
    #[error("base URL must be an absolute http or https URL with a host")]
    Invalid,
    /// A scheme other than `http` or `https` was supplied.
    #[error("base URL must use http or https")]
    UnsupportedScheme,
    /// The URL embeds `user:password@`.
    #[error("base URL must not contain credentials")]
    UserInfo,
    /// The URL contains a query or fragment.
    #[error("base URL must not contain a query or fragment")]
    QueryOrFragment,
    /// The path is neither empty nor `/v1`.
    #[error("base URL path must be empty or /v1")]
    UnsupportedPath,
}

/// Why an OpenObserve connection cannot be used.
///
/// Messages never include the entered values, in particular not the password.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ObservabilityConfigError {
    /// The URL was empty after trimming.
    #[error("observability base URL must not be empty")]
    EmptyBaseUrl,
    /// The value is not an absolute URL with a host.
    #[error("observability base URL must be an absolute http or https URL with a host")]
    InvalidBaseUrl,
    /// A scheme other than `http` or `https` was supplied.
    #[error("observability base URL must use http or https")]
    UnsupportedScheme,
    /// The URL embeds `user:password@`.
    #[error("observability base URL must not contain credentials")]
    UserInfo,
    /// The URL contains a query or fragment.
    #[error("observability base URL must not contain a query or fragment")]
    QueryOrFragment,
    /// The organization was blank.
    #[error("organization must not be empty")]
    EmptyOrg,
    /// The user was blank.
    #[error("user must not be empty")]
    EmptyUser,
    /// The password was blank.
    #[error("password must not be empty")]
    EmptyPassword,
}

/// A validated OpenObserve connection.
///
/// Fields are private so every value goes through [`ObservabilityConfig::new`],
/// which normalizes the base URL. `Debug` never prints the password.
#[derive(Clone, PartialEq, Eq)]
pub struct ObservabilityConfig {
    base_url: String,
    org: String,
    user: String,
    password: String,
}

impl ObservabilityConfig {
    /// Validate and normalize an OpenObserve connection.
    ///
    /// The base URL is trimmed, restricted to `http`/`https`, stripped of a
    /// trailing slash, and rejected when it carries userinfo, a query, or a
    /// fragment. Organization and user are trimmed; the password is kept
    /// verbatim but must not be blank.
    pub fn new(
        base_url: &str,
        org: &str,
        user: &str,
        password: &str,
    ) -> Result<Self, ObservabilityConfigError> {
        let base_url = normalize_observability_base_url(base_url)?;
        let org = org.trim();
        if org.is_empty() {
            return Err(ObservabilityConfigError::EmptyOrg);
        }
        let user = user.trim();
        if user.is_empty() {
            return Err(ObservabilityConfigError::EmptyUser);
        }
        if password.trim().is_empty() {
            return Err(ObservabilityConfigError::EmptyPassword);
        }
        Ok(Self {
            base_url,
            org: org.to_owned(),
            user: user.to_owned(),
            password: password.to_owned(),
        })
    }

    /// Normalized origin (plus optional path prefix) without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// OpenObserve organization.
    #[must_use]
    pub fn org(&self) -> &str {
        &self.org
    }

    /// OpenObserve user. Callers must not display the password.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// OpenObserve password. Callers must never log or display this value.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for ObservabilityConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservabilityConfig")
            .field("base_url", &self.base_url)
            .field("org", &self.org)
            .field("user", &self.user)
            .field("password", &REDACTED_KEY)
            .finish()
    }
}

/// A normalized gateway origin plus its deployment key.
///
/// Fields are private so every value goes through [`Config::new`], which
/// normalizes the origin. `Debug` never prints the key.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    base_url: String,
    api_key: String,
    observability: Option<ObservabilityConfig>,
}

impl Config {
    /// Validate and normalize credentials.
    ///
    /// The base URL is trimmed, restricted to `http`/`https`, stripped of a
    /// trailing slash and an optional `/v1` prefix, and reduced to its origin.
    /// An empty API key is rejected before any network call.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ConfigError> {
        let base_url = normalize_base_url(base_url)?;
        if api_key.trim().is_empty() {
            return Err(ConfigError::EmptyApiKey);
        }
        Ok(Self {
            base_url,
            api_key: api_key.to_owned(),
            observability: None,
        })
    }

    /// Attach a validated OpenObserve connection.
    ///
    /// Kept separate from [`Config::new`] so every existing call site keeps
    /// working and a missing `[observability]` section stays representable.
    #[must_use]
    pub fn with_observability(mut self, observability: ObservabilityConfig) -> Self {
        self.observability = Some(observability);
        self
    }

    /// Normalized origin without a trailing slash, e.g. `http://127.0.0.1:8080`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Deployment API key. Callers must never log or display the returned value.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// OpenObserve connection, when configured.
    #[must_use]
    pub fn observability(&self) -> Option<&ObservabilityConfig> {
        self.observability.as_ref()
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_url", &self.base_url)
            .field("api_key", &REDACTED_KEY)
            .field("observability", &self.observability)
            .finish()
    }
}

/// Failure while reading, validating or writing credentials.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `dirs::home_dir()` returned nothing.
    #[error("home directory is not available")]
    HomeDirUnavailable,
    /// The base URL failed normalization.
    #[error("base URL is invalid: {0}")]
    InvalidBaseUrl(#[from] BaseUrlError),
    /// The key was blank.
    #[error("API key must not be empty")]
    EmptyApiKey,
    /// The credential directory could not be created or secured.
    #[error("failed to prepare config directory {path}: {source}")]
    CreateDir {
        /// Directory that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file could not be read.
    #[error("failed to read config file {path}: {source}")]
    Read {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML or has an unexpected shape.
    ///
    /// The parser error is deliberately not retained: its display text can
    /// include the source line, which contains the API key.
    #[error("config file {path} is not valid TOML")]
    Corrupt {
        /// File that was rejected.
        path: PathBuf,
    },
    /// The file parsed but its values are invalid.
    #[error("config file {path} is invalid: {reason}")]
    Invalid {
        /// File that was rejected.
        path: PathBuf,
        /// Safe reason (never contains the key).
        reason: InvalidConfig,
    },
    /// The file could not be written atomically.
    #[error("failed to write config file {path}: {source}")]
    Write {
        /// Destination path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file could not be deleted.
    #[error("failed to delete config file {path}: {source}")]
    Delete {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Safe reason for a config file whose values are invalid.
///
/// The wording mirrors [`ConfigError`]'s `Display` so the diagnostic is stable;
/// the UI maps it through `text::config_error_detail`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidConfig {
    /// The saved base URL failed validation.
    #[error("base URL is invalid: {0}")]
    BaseUrl(#[from] BaseUrlError),
    /// The saved key was blank.
    #[error("API key must not be empty")]
    ApiKey,
    /// The saved observability section failed validation.
    #[error("observability section is invalid: {0}")]
    Observability(#[from] ObservabilityConfigError),
}

/// A non-fatal observation made while loading the config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigWarning {
    /// The file is readable or writable by someone other than the owner.
    PermissionsTooOpen {
        /// Permission bits reported by the filesystem.
        mode: u32,
    },
}

/// A loaded config plus non-fatal warnings.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    config: Config,
    warnings: Vec<ConfigWarning>,
}

impl LoadedConfig {
    fn new(config: Config, warnings: Vec<ConfigWarning>) -> Self {
        Self { config, warnings }
    }

    /// The validated credentials.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Non-fatal observations, e.g. loose file permissions.
    #[must_use]
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// Consume the wrapper.
    #[must_use]
    pub fn into_parts(self) -> (Config, Vec<ConfigWarning>) {
        (self.config, self.warnings)
    }
}

/// The on-disk TOML shape. Kept private so no other type can serialize the key.
///
/// Deliberately not `Debug`: the derived form would print the plaintext key and
/// the OpenObserve password.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    base_url: String,
    api_key: String,
    /// Optional so configs written before observability existed load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observability: Option<ObservabilityFileConfig>,
}

/// On-disk shape of the optional `[observability]` table.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservabilityFileConfig {
    base_url: String,
    org: String,
    user: String,
    password: String,
}

/// A credential file at a concrete path.
///
/// Tests and future alternate profile support can build a store at any path;
/// the production constructor is [`ConfigStore::from_home`].
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Use `~/.config/peri-swarm-dashboard/config.toml`.
    pub fn from_home() -> Result<Self, ConfigError> {
        Ok(Self::at(config_path()?))
    }

    /// Use an explicit file path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// File managed by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the file.
    ///
    /// Returns `Ok(None)` when the file does not exist. A parse or validation
    /// failure is an error; callers must not overwrite the file automatically.
    pub fn load(&self) -> Result<Option<LoadedConfig>, ConfigError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let file: FileConfig = toml::from_str(&text).map_err(|_| ConfigError::Corrupt {
            path: self.path.clone(),
        })?;
        let mut config = match Config::new(&file.base_url, &file.api_key) {
            Ok(config) => config,
            Err(ConfigError::InvalidBaseUrl(reason)) => {
                return Err(ConfigError::Invalid {
                    path: self.path.clone(),
                    reason: InvalidConfig::BaseUrl(reason),
                });
            }
            Err(ConfigError::EmptyApiKey) => {
                return Err(ConfigError::Invalid {
                    path: self.path.clone(),
                    reason: InvalidConfig::ApiKey,
                });
            }
            Err(other) => return Err(other),
        };
        if let Some(observability) = file.observability {
            let observability = ObservabilityConfig::new(
                &observability.base_url,
                &observability.org,
                &observability.user,
                &observability.password,
            )
            .map_err(|reason| ConfigError::Invalid {
                path: self.path.clone(),
                reason: InvalidConfig::Observability(reason),
            })?;
            config = config.with_observability(observability);
        }
        Ok(Some(LoadedConfig::new(config, self.permission_warnings())))
    }

    /// Write the file atomically with mode `0600` and directory mode `0700`.
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        let file = FileConfig {
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            observability: config.observability.as_ref().map(|observability| {
                ObservabilityFileConfig {
                    base_url: observability.base_url.clone(),
                    org: observability.org.clone(),
                    user: observability.user.clone(),
                    password: observability.password.clone(),
                }
            }),
        };
        let text = toml::to_string_pretty(&file).map_err(|error| ConfigError::Write {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
        })?;
        let directory = self.path.parent().ok_or_else(|| ConfigError::Write {
            path: self.path.clone(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            ),
        })?;
        fs::create_dir_all(directory).map_err(|source| ConfigError::CreateDir {
            path: directory.to_path_buf(),
            source,
        })?;
        set_directory_mode(directory)?;

        let mut temp = tempfile::Builder::new()
            .prefix(".config-")
            .suffix(".toml")
            .tempfile_in(directory)
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        set_file_mode(temp.path())?;
        temp.write_all(text.as_bytes())
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        temp.as_file()
            .sync_all()
            .map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        temp.persist(&self.path)
            .map_err(|error| ConfigError::Write {
                path: self.path.clone(),
                source: error.error,
            })?;
        // Re-assert the mode after rename; the temp file was already 0600.
        set_file_mode(&self.path)
    }

    /// Delete the credential file. Returns whether a file was removed.
    pub fn delete(&self) -> Result<bool, ConfigError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(ConfigError::Delete {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn permission_warnings(&self) -> Vec<ConfigWarning> {
        #[cfg(unix)]
        {
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return vec![ConfigWarning::PermissionsTooOpen { mode }];
                }
            }
        }
        Vec::new()
    }
}

/// `$HOME/.config/peri-swarm-dashboard/config.toml`.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let home = dirs::home_dir().ok_or(ConfigError::HomeDirUnavailable)?;
    Ok(config_path_in(&home))
}

/// Same path for an explicit home directory, without touching the environment.
#[must_use]
pub fn config_path_in(home: &Path) -> PathBuf {
    home.join(".config")
        .join(CONFIG_DIR_NAME)
        .join(CONFIG_FILE_NAME)
}

/// Normalize an operator-entered OpenObserve base URL.
///
/// Unlike the gateway origin, a path prefix is allowed (OpenObserve may sit
/// behind a reverse-proxy prefix). Any trailing slash is removed.
pub fn normalize_observability_base_url(input: &str) -> Result<String, ObservabilityConfigError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ObservabilityConfigError::EmptyBaseUrl);
    }
    let url = Url::parse(trimmed).map_err(|_| ObservabilityConfigError::InvalidBaseUrl)?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(ObservabilityConfigError::UnsupportedScheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ObservabilityConfigError::UserInfo);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ObservabilityConfigError::QueryOrFragment);
    }
    if url.host_str().is_none() {
        return Err(ObservabilityConfigError::InvalidBaseUrl);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Normalize an operator-entered base URL to a bare origin.
pub fn normalize_base_url(input: &str) -> Result<String, BaseUrlError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(BaseUrlError::Empty);
    }
    let url = Url::parse(trimmed).map_err(|_| BaseUrlError::Invalid)?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(BaseUrlError::UnsupportedScheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(BaseUrlError::UserInfo);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(BaseUrlError::QueryOrFragment);
    }
    if url.host_str().is_none() {
        return Err(BaseUrlError::Invalid);
    }
    let path = url.path();
    if !matches!(path, "" | "/" | "/v1" | "/v1/") {
        return Err(BaseUrlError::UnsupportedPath);
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(unix)]
fn set_directory_mode(path: &Path) -> Result<(), ConfigError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        ConfigError::CreateDir {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path) -> Result<(), ConfigError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        ConfigError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
