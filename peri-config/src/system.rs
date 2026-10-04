use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    app::PeriConfig,
    assembly::{self, parse_document},
    mcp::{McpCachePolicy, McpConfigFile},
    observability::LangfuseConfig,
    provider::ResolvedProvider,
    resources::ResourceConfiguration,
    source::{ConfigurationSource, McpConfigurationSource},
    ui::TuiConfig,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConfigurationScope {
    pub cwd: PathBuf,
    pub global_settings: PathBuf,
}

impl ConfigurationScope {
    pub fn new(cwd: PathBuf, global_settings: PathBuf) -> Result<Self, ConfigurationError> {
        let scope = Self {
            cwd,
            global_settings,
        };
        scope.validate()?;
        Ok(scope)
    }

    fn validate(&self) -> Result<(), ConfigurationError> {
        if !self.cwd.is_absolute() || !self.global_settings.is_absolute() {
            return Err(ConfigurationError::InvalidScope);
        }
        Ok(())
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationInputs {
    pub global: Option<String>,
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub environment: BTreeMap<String, String>,
}

impl fmt::Debug for ConfigurationInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigurationInputs")
            .field("global_present", &self.global.is_some())
            .field("workspace_present", &self.workspace.is_some())
            .field("project_present", &self.project.is_some())
            .field("environment_keys", &self.environment.keys())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigurationRevision([u8; 32]);

impl fmt::Debug for ConfigurationRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfigurationRevision(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIdentity {
    Defaults,
    GlobalFile,
    WorkspaceFile,
    ProjectMcpFile,
    Environment(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationField {
    Settings,
    McpServers,
    McpCache,
    BuiltinMcp,
    Provider,
    Observability,
    Ui,
    Resources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldExplanation {
    pub field: ConfigurationField,
    pub revision: ConfigurationRevision,
    pub contributors: Vec<SourceIdentity>,
    pub rule: &'static str,
    pub contains_sensitive_values: bool,
}

#[derive(Debug, Error)]
pub enum ConfigurationError {
    #[error("configuration scope requires absolute paths")]
    InvalidScope,
    #[error("configuration input I/O failed")]
    InputUnavailable,
    #[error("invalid configuration in {source_identity:?} ({domain})")]
    InvalidInput {
        source_identity: SourceIdentity,
        domain: &'static str,
    },
    #[error("configuration revision conflict; reload before saving")]
    Conflict,
    #[error("configuration scope has not been resolved")]
    UnresolvedScope,
    #[error("configuration update could not be serialized")]
    Serialization,
}

#[derive(Clone)]
pub struct ConfigurationSnapshot {
    scope: ConfigurationScope,
    revision: ConfigurationRevision,
    inputs: ConfigurationInputs,
    settings: PeriConfig,
    global_settings: PeriConfig,
    mcp: McpConfigFile,
    effective_provider: Option<ResolvedProvider>,
    observability: LangfuseConfig,
    ui: TuiConfig,
    resources: ResourceConfiguration,
}

impl fmt::Debug for ConfigurationSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigurationSnapshot")
            .field("scope", &self.scope)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl ConfigurationSnapshot {
    pub fn resolve(
        scope: ConfigurationScope,
        inputs: ConfigurationInputs,
    ) -> Result<Self, ConfigurationError> {
        scope.validate()?;
        let resolved = assembly::resolve(&scope, &inputs)?;
        let revision = revision_of(&scope, &inputs)?;
        Ok(Self {
            scope,
            revision,
            inputs,
            settings: resolved.settings,
            global_settings: resolved.global_settings,
            mcp: resolved.mcp,
            effective_provider: resolved.provider,
            observability: resolved.observability,
            ui: resolved.ui,
            resources: resolved.resources,
        })
    }

    pub fn scope(&self) -> &ConfigurationScope {
        &self.scope
    }

    pub fn revision(&self) -> ConfigurationRevision {
        self.revision
    }

    pub fn settings(&self) -> &PeriConfig {
        &self.settings
    }

    pub(crate) fn global_settings(&self) -> &PeriConfig {
        &self.global_settings
    }

    pub(crate) fn inputs(&self) -> &ConfigurationInputs {
        &self.inputs
    }

    pub fn mcp(&self) -> &McpConfigFile {
        &self.mcp
    }

    pub fn mcp_with_plugins(
        &self,
        plugins: &HashMap<String, peri_acp_types::plugin::McpServerConfig>,
    ) -> Result<McpConfigFile, ConfigurationError> {
        assembly::resolve_mcp(&self.scope, &self.inputs, plugins)
    }

    pub fn bare_mcp(&self) -> Result<McpConfigFile, ConfigurationError> {
        crate::mcp::resolve(
            &serde_json::json!({}),
            &serde_json::json!({}),
            &HashMap::new(),
            &self.inputs.environment,
        )
        .map_err(|_| ConfigurationError::InvalidInput {
            source_identity: SourceIdentity::Environment(crate::mcp::MCP_CACHE_ENV.to_owned()),
            domain: "MCP cache",
        })
    }

    pub fn cache_policy(&self) -> McpCachePolicy {
        McpCachePolicy::from_setting(self.mcp.mcp_cache)
    }

    pub fn provider(&self) -> Option<&ResolvedProvider> {
        self.effective_provider.as_ref()
    }

    pub fn observability(&self) -> &LangfuseConfig {
        &self.observability
    }

    pub fn ui(&self) -> &TuiConfig {
        &self.ui
    }

    pub fn resources(&self) -> &ResourceConfiguration {
        &self.resources
    }

    pub fn builtin_mcp_enabled(&self) -> bool {
        crate::mcp::builtin_enabled(&self.inputs.environment)
    }

    pub fn explain(&self, field: ConfigurationField) -> FieldExplanation {
        assembly::explain(field, self.revision, &self.inputs)
    }
}

pub struct ConfigurationSystem {
    source: Arc<dyn ConfigurationSource>,
    snapshots: Mutex<HashMap<ConfigurationScope, Arc<ConfigurationSnapshot>>>,
}

impl Default for ConfigurationSystem {
    fn default() -> Self {
        Self::new(Arc::new(McpConfigurationSource))
    }
}

impl ConfigurationSystem {
    pub fn new(source: Arc<dyn ConfigurationSource>) -> Self {
        Self {
            source,
            snapshots: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(
        &self,
        scope: ConfigurationScope,
    ) -> Result<Arc<ConfigurationSnapshot>, ConfigurationError> {
        scope.validate()?;
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let inputs = self
            .source
            .collect(&scope)
            .map_err(|_| ConfigurationError::InputUnavailable)?;
        let snapshot = Arc::new(ConfigurationSnapshot::resolve(scope.clone(), inputs)?);
        snapshots.insert(scope, snapshot.clone());
        Ok(snapshot)
    }

    pub fn current(&self, scope: &ConfigurationScope) -> Option<Arc<ConfigurationSnapshot>> {
        self.snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(scope)
            .cloned()
    }

    pub fn explain(
        &self,
        scope: &ConfigurationScope,
        field: ConfigurationField,
    ) -> Result<FieldExplanation, ConfigurationError> {
        self.current(scope)
            .map(|snapshot| snapshot.explain(field))
            .ok_or(ConfigurationError::UnresolvedScope)
    }

    pub fn update(
        &self,
        scope: &ConfigurationScope,
        expected_revision: ConfigurationRevision,
        settings: &PeriConfig,
    ) -> Result<Arc<ConfigurationSnapshot>, ConfigurationError> {
        self.update_document(scope, expected_revision, |current, inputs| {
            let workspace = inputs.workspace.is_some();
            let layer = if workspace {
                PeriConfig {
                    schema: settings.schema.clone(),
                    config: settings
                        .config
                        .extract_overrides(&current.global_settings.config),
                }
            } else {
                settings.clone()
            };
            let content = if workspace {
                &mut inputs.workspace
            } else {
                &mut inputs.global
            };
            let mut document = parse_document(
                content.as_deref(),
                if workspace {
                    SourceIdentity::WorkspaceFile
                } else {
                    SourceIdentity::GlobalFile
                },
            )?;
            let encoded =
                serde_json::to_value(layer).map_err(|_| ConfigurationError::Serialization)?;
            let mut settings_document = encoded["config"].clone();
            for key in ["mcpServers", "mcpCache"] {
                if let Some(original) = document.get("config").and_then(|config| config.get(key)) {
                    settings_document[key] = original.clone();
                } else if let Some(object) = settings_document.as_object_mut() {
                    object.remove(key);
                }
            }
            document["config"] = settings_document;
            if !workspace || document.get("$schema").is_none() {
                if let Some(schema) = encoded.get("$schema") {
                    document["$schema"] = schema.clone();
                }
            }
            let expected = content.clone();
            *content = Some(
                serde_json::to_string_pretty(&document)
                    .map_err(|_| ConfigurationError::Serialization)?,
            );
            let path = if workspace {
                assembly::workspace_settings_path(&scope.cwd)
            } else {
                scope.global_settings.clone()
            };
            Ok((path, expected, content.clone().expect("encoded document")))
        })
    }

    pub fn update_mcp(
        &self,
        scope: &ConfigurationScope,
        expected_revision: ConfigurationRevision,
        config: &McpConfigFile,
    ) -> Result<Arc<ConfigurationSnapshot>, ConfigurationError> {
        self.update_document(scope, expected_revision, |_, inputs| {
            let project_path = assembly::project_mcp_path(&scope.cwd);
            for server in config.mcp_servers.values() {
                match &server.source {
                    None => {}
                    Some(peri_acp_types::plugin::ConfigSource::Project(path))
                        if path == &project_path => {}
                    _ => {
                        return Err(ConfigurationError::InvalidInput {
                            source_identity: SourceIdentity::ProjectMcpFile,
                            domain: "project MCP update cannot import non-project servers",
                        })
                    }
                }
            }
            let mut document =
                parse_document(inputs.project.as_deref(), SourceIdentity::ProjectMcpFile)?;
            let encoded =
                serde_json::to_value(config).map_err(|_| ConfigurationError::Serialization)?;
            document["mcpServers"] = encoded["mcpServers"].clone();
            if let Some(cache) = encoded.get("mcpCache") {
                document["mcpCache"] = cache.clone();
            } else if let Some(object) = document.as_object_mut() {
                object.remove("mcpCache");
            }
            let expected = inputs.project.clone();
            let content = serde_json::to_string_pretty(&document)
                .map_err(|_| ConfigurationError::Serialization)?;
            inputs.project = Some(content.clone());
            Ok((assembly::project_mcp_path(&scope.cwd), expected, content))
        })
    }

    fn update_document(
        &self,
        scope: &ConfigurationScope,
        expected_revision: ConfigurationRevision,
        changes: impl FnOnce(
            &ConfigurationSnapshot,
            &mut ConfigurationInputs,
        ) -> Result<(PathBuf, Option<String>, String), ConfigurationError>,
    ) -> Result<Arc<ConfigurationSnapshot>, ConfigurationError> {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let current = snapshots
            .get(scope)
            .ok_or(ConfigurationError::UnresolvedScope)?;
        if current.revision != expected_revision {
            return Err(ConfigurationError::Conflict);
        }
        let mut inputs = self
            .source
            .collect(scope)
            .map_err(|_| ConfigurationError::InputUnavailable)?;
        if revision_of(scope, &inputs)? != expected_revision {
            return Err(ConfigurationError::Conflict);
        }
        let (path, expected, content) = changes(current, &mut inputs)?;
        let next = Arc::new(ConfigurationSnapshot::resolve(scope.clone(), inputs)?);
        if !self
            .source
            .write_if_unchanged(&path, expected.as_deref(), &content)
            .map_err(|_| ConfigurationError::InputUnavailable)?
        {
            return Err(ConfigurationError::Conflict);
        }
        snapshots.retain(|other_scope, _| {
            other_scope.global_settings != scope.global_settings || other_scope == scope
        });
        snapshots.insert(scope.clone(), next.clone());
        Ok(next)
    }
}

fn revision_of(
    scope: &ConfigurationScope,
    inputs: &ConfigurationInputs,
) -> Result<ConfigurationRevision, ConfigurationError> {
    let encoded = serde_json::to_vec(inputs).map_err(|_| ConfigurationError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(b"peri-configuration-v1");
    for path in [&scope.cwd, &scope.global_settings] {
        let bytes = path.as_os_str().as_encoded_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.update(encoded);
    Ok(ConfigurationRevision(digest.finalize().into()))
}

#[cfg(test)]
#[path = "system_test.rs"]
mod tests;
