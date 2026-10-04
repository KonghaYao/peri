use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::Value;
use thiserror::Error;

use crate::app::PeriConfig;
use crate::source::{ConfigurationSource, McpConfigurationSource};
use crate::{ConfigurationScope, ConfigurationSnapshot, ConfigurationSystem};

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("configuration input I/O failed")]
    Input(#[from] std::io::Error),
    #[error("configuration JSON must be a valid object")]
    InvalidJson,
    #[error(transparent)]
    Resolution(#[from] crate::ConfigurationError),
    #[error("configuration authority unavailable; reload the configuration source")]
    AuthorityUnavailable,
    #[error("Global configuration changed; restart Peri before saving workspace settings")]
    GlobalBaselineChanged,
    #[error("configuration revision conflict; reload before saving")]
    Conflict,
}

pub type Result<Output> = std::result::Result<Output, SettingsError>;

struct FixedLayoutSource {
    workspace: Option<PathBuf>,
    project: Option<PathBuf>,
    injected_global: Option<String>,
}

impl ConfigurationSource for FixedLayoutSource {
    fn collect(&self, scope: &ConfigurationScope) -> std::io::Result<crate::ConfigurationInputs> {
        crate::source::collect_with_layout_and_global(
            scope,
            self.workspace.as_deref(),
            self.project.as_deref(),
            self.injected_global.as_deref(),
        )
    }

    fn write_if_unchanged(
        &self,
        path: &Path,
        expected: Option<&str>,
        content: &str,
    ) -> std::io::Result<bool> {
        if self.injected_global.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected settings are read-only",
            ));
        }
        McpConfigurationSource.write_if_unchanged(path, expected, content)
    }
}

pub fn config_path() -> PathBuf {
    peri_mcp_config::global_config_path()
}

pub fn set_global_config_path(path: Option<PathBuf>) {
    peri_mcp_config::set_global_config_path(path);
}

fn workspace_config_path_at(cwd: &Path, global_path: &Path) -> Result<Option<PathBuf>> {
    let path = cwd.join(".peri").join("settings.json");
    if !peri_mcp_config::exists(&path)? || peri_mcp_config::same_file(&path, global_path)? {
        return Ok(None);
    }
    Ok(Some(path))
}

pub fn workspace_config_path() -> Option<PathBuf> {
    let cwd = peri_mcp_config::current_dir().ok()?;
    workspace_config_path_at(&cwd, &config_path()).unwrap_or_else(|error| {
        tracing::warn!(error = %error, "工作区配置路径探测失败");
        None
    })
}

pub struct ConfigSource {
    global_path: PathBuf,
    workspace_path: Option<PathBuf>,
    cwd: PathBuf,
    global: PeriConfig,
    merged: PeriConfig,
    raw_global: Option<String>,
    raw_workspace: Option<String>,
    layout_error: Option<SettingsError>,
    authority: Option<ConfigurationSystem>,
    scope: Option<ConfigurationScope>,
}

impl ConfigSource {
    pub fn load_at(cwd: &Path, global_path: PathBuf) -> Result<Self> {
        let workspace_path = workspace_config_path_at(cwd, &global_path)?;
        Self::load_layout(
            cwd,
            global_path,
            workspace_path,
            Some(cwd.join(".mcp.json")),
            None,
        )
    }

    /// Resolve a complete settings document supplied by a trusted process launcher.
    /// This source never reads or writes global/workspace settings files.
    pub fn load_injected_at(cwd: &Path, global_path: PathBuf, settings: String) -> Result<Self> {
        Self::load_layout(
            cwd,
            global_path,
            None,
            Some(cwd.join(".mcp.json")),
            Some(settings),
        )
    }

    fn load_layout(
        cwd: &Path,
        global_path: PathBuf,
        workspace_path: Option<PathBuf>,
        project_path: Option<PathBuf>,
        injected_global: Option<String>,
    ) -> Result<Self> {
        let scope = ConfigurationScope::new(cwd.to_owned(), global_path.clone())?;
        let authority = ConfigurationSystem::new(Arc::new(FixedLayoutSource {
            workspace: workspace_path.clone(),
            project: project_path,
            injected_global,
        }));
        let snapshot = authority.resolve(scope.clone())?;
        Ok(Self {
            global_path,
            workspace_path,
            cwd: cwd.to_owned(),
            global: snapshot.global_settings().clone(),
            merged: snapshot.settings().clone(),
            raw_global: snapshot.inputs().global.clone(),
            raw_workspace: snapshot.inputs().workspace.clone(),
            layout_error: None,
            authority: Some(authority),
            scope: Some(scope),
        })
    }

    pub fn load() -> Result<Self> {
        let cwd = peri_mcp_config::current_dir()?;
        Self::load_at(&cwd, config_path())
    }

    pub fn load_lenient() -> Self {
        match peri_mcp_config::current_dir() {
            Ok(cwd) => Self::load_at_lenient(&cwd, config_path()),
            Err(error) => {
                tracing::warn!(error = %error, "无法获取当前工作目录，配置不可写");
                let global_path = config_path();
                let (global, raw_global) = load_with_raw(&global_path).unwrap_or_else(|error| {
                    tracing::warn!(path = %global_path.display(), error = %error, "全局配置解析失败，按空配置继续");
                    (PeriConfig::default(), None)
                });
                Self {
                    global_path,
                    workspace_path: None,
                    cwd: PathBuf::new(),
                    merged: global.clone(),
                    global,
                    raw_global,
                    raw_workspace: None,
                    layout_error: Some(error.into()),
                    authority: None,
                    scope: None,
                }
            }
        }
    }

    pub fn load_standalone(path: PathBuf) -> Result<Self> {
        let path = if path.is_absolute() {
            path
        } else {
            peri_mcp_config::current_dir()?.join(path)
        };
        let cwd = peri_mcp_config::current_dir()?;
        Self::load_layout(&cwd, path, None, None, None)
    }

    /// Resolve one inline settings document without reading settings or project files.
    /// The path only identifies the configuration scope; this source is read-only.
    pub fn load_standalone_inline_at(
        cwd: &Path,
        global_path: PathBuf,
        settings: String,
    ) -> Result<Self> {
        Self::load_layout(cwd, global_path, None, None, Some(settings))
    }

    pub fn load_at_lenient(cwd: &Path, global_path: PathBuf) -> Self {
        let authority_error = match Self::load_at(cwd, global_path.clone()) {
            Ok(source) => return source,
            Err(error) => error,
        };
        let (workspace_path, layout_error) = match workspace_config_path_at(cwd, &global_path) {
            Ok(path) => (path, None),
            Err(error) => {
                tracing::warn!(error = %error, "工作区配置路径探测失败，配置不可写");
                (None, Some(error))
            }
        };
        let (global, raw_global) = load_with_raw(&global_path).unwrap_or_else(|error| {
            tracing::warn!(path = %global_path.display(), error = %error, "全局配置解析失败，按空配置继续");
            (PeriConfig::default(), None)
        });
        let (workspace, raw_workspace) = match workspace_path.as_deref() {
            Some(path) => match load_with_raw(path) {
                Ok((workspace, raw)) => (Some(workspace), raw),
                Err(error) => {
                    tracing::warn!(error = %error, "工作区配置解析失败，按空配置继续");
                    (None, None)
                }
            },
            None => (None, None),
        };
        let mut merged = global.clone();
        if let Some(workspace) = &workspace {
            merged.config.merge_overrides(workspace.config.clone());
        }
        Self {
            global_path,
            workspace_path,
            cwd: cwd.to_owned(),
            global,
            merged,
            raw_global,
            raw_workspace,
            layout_error: layout_error.or(Some(authority_error)),
            authority: None,
            scope: None,
        }
    }

    pub fn global_path(&self) -> &Path {
        &self.global_path
    }

    pub fn workspace_path(&self) -> Option<&Path> {
        self.workspace_path.as_deref()
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn raw_global(&self) -> Option<&str> {
        self.raw_global.as_deref()
    }

    /// Resource policy from this selected source, including a lenient source
    /// without a published snapshot. Missing or invalid source data has no
    /// trustworthy policy and must be handled by the caller explicitly.
    pub fn resource_configuration(&self) -> Option<crate::resources::ResourceConfiguration> {
        if let Some(snapshot) = self.snapshot() {
            return Some(*snapshot.resources());
        }
        let document: Value = serde_json::from_str(self.raw_global.as_deref()?).ok()?;
        Some(crate::resources::resolve(&document))
    }

    pub fn raw_workspace(&self) -> Option<&str> {
        self.raw_workspace.as_deref()
    }

    pub fn is_workspace(&self) -> bool {
        self.workspace_path.is_some()
    }

    pub fn global_config(&self) -> &PeriConfig {
        &self.global
    }

    pub fn loaded_merged(&self) -> PeriConfig {
        self.snapshot()
            .map(|snapshot| snapshot.settings().clone())
            .unwrap_or_else(|| self.merged.clone())
    }

    pub fn snapshot(&self) -> Option<Arc<ConfigurationSnapshot>> {
        self.authority.as_ref()?.current(self.scope.as_ref()?)
    }

    pub fn reload_merged(&self) -> Result<PeriConfig> {
        if self.layout_error.is_some() {
            return Err(SettingsError::AuthorityUnavailable);
        }
        if let (Some(authority), Some(scope)) = (&self.authority, &self.scope) {
            return Ok(authority.resolve(scope.clone())?.settings().clone());
        }
        Err(SettingsError::AuthorityUnavailable)
    }

    pub fn save(
        &self,
        expected_revision: crate::ConfigurationRevision,
        merged: &PeriConfig,
    ) -> Result<Arc<ConfigurationSnapshot>> {
        if self.layout_error.is_some() {
            return Err(SettingsError::AuthorityUnavailable);
        }
        if let (Some(authority), Some(scope), Some(snapshot)) =
            (&self.authority, &self.scope, self.snapshot())
        {
            if self.workspace_path.is_some() && snapshot.global_settings() != &self.global {
                return Err(SettingsError::GlobalBaselineChanged);
            }
            return Ok(authority.update(scope, expected_revision, merged)?);
        }
        Err(SettingsError::AuthorityUnavailable)
    }
}

pub fn load() -> Result<PeriConfig> {
    Ok(ConfigSource::load()?.loaded_merged())
}

pub fn load_from(path: &Path) -> Result<PeriConfig> {
    load_with_raw(path).map(|(config, _)| config)
}

fn load_with_raw(path: &Path) -> Result<(PeriConfig, Option<String>)> {
    if !peri_mcp_config::exists(path)? {
        return Ok((PeriConfig::default(), None));
    }
    let content = peri_mcp_config::read_text(path)?;
    let document: Value = serde_json::from_str(&content).map_err(|_| SettingsError::InvalidJson)?;
    if !document.is_object() {
        return Err(SettingsError::InvalidJson);
    }
    let mut config: PeriConfig =
        serde_json::from_value(document).map_err(|_| SettingsError::InvalidJson)?;
    config.config.validate_meta_harness();
    Ok((config, Some(content)))
}

pub fn save_to(config: &PeriConfig, path: &Path) -> Result<()> {
    let raw = match peri_mcp_config::read_text(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    save_preserving_siblings(config, path, raw.as_deref())
}

fn save_preserving_siblings(config: &PeriConfig, path: &Path, raw: Option<&str>) -> Result<()> {
    let mut serialized = serde_json::to_value(config).map_err(|_| SettingsError::InvalidJson)?;
    if let (Some(raw), Some(serialized)) = (raw, serialized.as_object_mut()) {
        let existing: Value = serde_json::from_str(raw).map_err(|_| SettingsError::InvalidJson)?;
        let Value::Object(mut existing) = existing else {
            return Err(SettingsError::InvalidJson);
        };
        for key in serialized.keys() {
            existing.remove(key);
        }
        existing.extend(serialized.clone());
        *serialized = existing;
    }
    let content =
        serde_json::to_string_pretty(&serialized).map_err(|_| SettingsError::InvalidJson)?;
    if !peri_mcp_config::write_text_if_unchanged(path, &raw.map(str::to_owned), &content)? {
        return Err(SettingsError::Conflict);
    }
    Ok(())
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;

#[cfg(test)]
#[path = "settings_revision_test.rs"]
mod revision_tests;
