use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    app::PeriConfig,
    mcp::{McpCachePolicy, McpConfigFile},
    observability::LangfuseConfig,
    provider::{EnvironmentProvider, ResolvedProvider},
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
    provider: Option<EnvironmentProvider>,
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
        let global = parse_document(inputs.global.as_deref(), SourceIdentity::GlobalFile)?;
        let workspace = parse_document(inputs.workspace.as_deref(), SourceIdentity::WorkspaceFile)?;
        let project = parse_document(inputs.project.as_deref(), SourceIdentity::ProjectMcpFile)?;
        let global_settings = parse_settings(&global, SourceIdentity::GlobalFile)?;
        let mut settings = global_settings.clone();
        if inputs.workspace.is_some() {
            let overrides = parse_settings(&workspace, SourceIdentity::WorkspaceFile)?;
            settings.config.merge_overrides(overrides.config);
        }
        let mut global_mcp =
            crate::mcp::parse_global(&global).map_err(|_| ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::GlobalFile,
                domain: "MCP",
            })?;
        for server in global_mcp.mcp_servers.values_mut() {
            server.source = Some(peri_acp_types::plugin::ConfigSource::Global(
                scope.global_settings.clone(),
            ));
        }
        let mut project_mcp =
            crate::mcp::parse_project(&project).map_err(|_| ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::ProjectMcpFile,
                domain: "MCP",
            })?;
        for server in project_mcp.mcp_servers.values_mut() {
            server.source = Some(peri_acp_types::plugin::ConfigSource::Project(
                scope.cwd.join(".mcp.json"),
            ));
        }
        let mcp = crate::mcp::resolve_from_files(
            &global_mcp,
            &project_mcp,
            &HashMap::new(),
            &inputs.environment,
        )
        .map_err(|_| ConfigurationError::InvalidInput {
            source_identity: SourceIdentity::Environment(crate::mcp::MCP_CACHE_ENV.to_owned()),
            domain: "MCP cache",
        })?;
        let provider = EnvironmentProvider::resolve(&inputs.environment);
        let effective_provider = crate::provider::resolve(&settings, &inputs.environment);
        let observability = crate::observability::resolve(&global, &inputs.environment);
        let ui = TuiConfig::from_extra(&settings.config.extra);
        let resources = crate::resources::resolve(&global);
        let revision = revision_of(&scope, &inputs)?;
        Ok(Self {
            scope,
            revision,
            inputs,
            settings,
            global_settings,
            mcp,
            provider,
            effective_provider,
            observability,
            ui,
            resources,
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
        let global = parse_document(self.inputs.global.as_deref(), SourceIdentity::GlobalFile)?;
        let project = parse_document(
            self.inputs.project.as_deref(),
            SourceIdentity::ProjectMcpFile,
        )?;
        let mut global =
            crate::mcp::parse_global(&global).map_err(|_| ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::GlobalFile,
                domain: "MCP",
            })?;
        let mut project =
            crate::mcp::parse_project(&project).map_err(|_| ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::ProjectMcpFile,
                domain: "MCP",
            })?;
        for server in global.mcp_servers.values_mut() {
            server.source = Some(peri_acp_types::plugin::ConfigSource::Global(
                self.scope.global_settings.clone(),
            ));
        }
        for server in project.mcp_servers.values_mut() {
            server.source = Some(peri_acp_types::plugin::ConfigSource::Project(
                self.scope.cwd.join(".mcp.json"),
            ));
        }
        crate::mcp::resolve_from_files(&global, &project, plugins, &self.inputs.environment)
            .map_err(|_| ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::Defaults,
                domain: "MCP plugin inputs",
            })
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

    pub fn environment_provider(&self) -> Option<&EnvironmentProvider> {
        self.provider.as_ref()
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
        let mut contributors = vec![SourceIdentity::Defaults];
        if self.inputs.global.is_some() && field != ConfigurationField::BuiltinMcp {
            contributors.push(SourceIdentity::GlobalFile);
        }
        match field {
            ConfigurationField::Settings
            | ConfigurationField::Ui
            | ConfigurationField::Provider => {
                if self.inputs.workspace.is_some() {
                    contributors.push(SourceIdentity::WorkspaceFile);
                }
            }
            ConfigurationField::McpServers | ConfigurationField::McpCache => {
                if self.inputs.project.is_some() {
                    contributors.push(SourceIdentity::ProjectMcpFile);
                }
            }
            ConfigurationField::Observability
            | ConfigurationField::Resources
            | ConfigurationField::BuiltinMcp => {}
        }
        let environment_keys: &[&str] = match field {
            ConfigurationField::McpCache => &[crate::mcp::MCP_CACHE_ENV],
            ConfigurationField::BuiltinMcp => &[crate::mcp::MCP_BUILTIN_ENV],
            ConfigurationField::Provider => crate::provider::ENVIRONMENT_KEYS,
            ConfigurationField::Observability => crate::observability::ENVIRONMENT_KEYS,
            _ => &[],
        };
        contributors.extend(
            environment_keys
                .iter()
                .filter(|key| self.inputs.environment.contains_key(**key))
                .map(|key| SourceIdentity::Environment((*key).to_owned())),
        );
        FieldExplanation {
            field,
            revision: self.revision,
            contributors,
            rule: match field {
                ConfigurationField::McpCache => {
                    "any false disables; true never relaxes another source"
                }
                ConfigurationField::McpServers => {
                    "global < plugin < project; manual namespaces deduplicate plugins"
                }
                ConfigurationField::Provider => {
                    "settings profiles first; environment provider is fallback only"
                }
                ConfigurationField::Observability => {
                    "global settings then named environment overrides"
                }
                ConfigurationField::BuiltinMcp => {
                    "off/0 disables runtime builtin injection; absent or unknown enables"
                }
                ConfigurationField::Resources => {
                    "global nested disableBundledSkills precedes top-level; default false"
                }
                _ => "workspace overrides by domain rules; profiles replace as a unit",
            },
            contains_sensitive_values: matches!(
                field,
                ConfigurationField::Settings
                    | ConfigurationField::McpServers
                    | ConfigurationField::Provider
                    | ConfigurationField::Observability
            ),
        }
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
                scope.cwd.join(".peri/settings.json")
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
            let project_path = scope.cwd.join(".mcp.json");
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
            Ok((scope.cwd.join(".mcp.json"), expected, content))
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

fn parse_document(
    content: Option<&str>,
    source_identity: SourceIdentity,
) -> Result<Value, ConfigurationError> {
    match content {
        None => Ok(serde_json::json!({})),
        Some(content) => {
            let document: Value =
                serde_json::from_str(content).map_err(|_| ConfigurationError::InvalidInput {
                    source_identity: source_identity.clone(),
                    domain: "JSON",
                })?;
            if !document.is_object() {
                return Err(ConfigurationError::InvalidInput {
                    source_identity,
                    domain: "JSON object",
                });
            }
            Ok(document)
        }
    }
}

fn parse_settings(
    document: &Value,
    source_identity: SourceIdentity,
) -> Result<PeriConfig, ConfigurationError> {
    let mut config: PeriConfig =
        serde_json::from_value(document.clone()).map_err(|_| ConfigurationError::InvalidInput {
            source_identity,
            domain: "settings",
        })?;
    config.config.validate_meta_harness();
    Ok(config)
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
