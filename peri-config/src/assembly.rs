//! The configuration entity: its sources, domain merge rules and consumer projections.
//! Source adapters collect bytes; this module alone assembles their meaning.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use serde_json::Value;

use crate::{
    app::PeriConfig,
    mcp::{self, McpConfigFile},
    observability::{self, LangfuseConfig},
    provider::{self, ResolvedProvider},
    resources::{self, ResourceConfiguration},
    system::{
        ConfigurationError, ConfigurationField, ConfigurationInputs, ConfigurationRevision,
        ConfigurationScope, FieldExplanation, SourceIdentity,
    },
    ui::TuiConfig,
};

#[derive(Clone, Copy)]
enum Source {
    Global,
    Workspace,
    // Added only by mcp_with_plugins; the base snapshot has no discovered plugin input.
    PluginMcp,
    ProjectMcp,
    Environment(&'static [&'static str]),
}

struct DomainShape {
    field: ConfigurationField,
    sources: &'static [Source],
    merge_rule: &'static str,
    sensitive: bool,
}

// Declare source participation once. Domain-specific merge functions below implement these
// rules; this table also drives input collection and the public provenance explanation.
const DOMAINS: &[DomainShape] = &[
    DomainShape {
        field: ConfigurationField::Settings,
        sources: &[Source::Global, Source::Workspace],
        merge_rule: "workspace overrides by domain rules; profiles replace as a unit",
        sensitive: true,
    },
    DomainShape {
        field: ConfigurationField::McpServers,
        sources: &[Source::Global, Source::PluginMcp, Source::ProjectMcp],
        merge_rule: "global < plugin < project; manual namespaces deduplicate plugins",
        sensitive: true,
    },
    DomainShape {
        field: ConfigurationField::McpCache,
        sources: &[
            Source::Global,
            Source::ProjectMcp,
            Source::Environment(&[mcp::MCP_CACHE_ENV]),
        ],
        merge_rule: "any false disables; true never relaxes another source",
        sensitive: false,
    },
    DomainShape {
        field: ConfigurationField::BuiltinMcp,
        sources: &[Source::Environment(&[mcp::MCP_BUILTIN_ENV])],
        merge_rule: "off/0 disables runtime builtin injection; absent or unknown enables",
        sensitive: false,
    },
    DomainShape {
        field: ConfigurationField::Provider,
        sources: &[
            Source::Global,
            Source::Workspace,
            Source::Environment(provider::ENVIRONMENT_KEYS),
        ],
        merge_rule: "settings profiles first; environment provider is fallback only",
        sensitive: true,
    },
    DomainShape {
        field: ConfigurationField::Observability,
        sources: &[
            Source::Global,
            Source::Environment(observability::ENVIRONMENT_KEYS),
        ],
        merge_rule: "global settings then named environment overrides",
        sensitive: true,
    },
    DomainShape {
        field: ConfigurationField::Ui,
        sources: &[Source::Global, Source::Workspace],
        merge_rule: "workspace overrides by domain rules; profiles replace as a unit",
        sensitive: false,
    },
    DomainShape {
        field: ConfigurationField::Resources,
        sources: &[Source::Global],
        merge_rule: "global nested disableBundledSkills precedes top-level; default false",
        sensitive: false,
    },
];

pub(crate) fn workspace_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".peri/settings.json")
}

pub(crate) fn project_mcp_path(cwd: &Path) -> PathBuf {
    cwd.join(".mcp.json")
}

pub(crate) fn environment_keys() -> Vec<&'static str> {
    let mut keys: Vec<_> = DOMAINS
        .iter()
        .flat_map(|domain| domain.sources)
        .filter_map(|source| match source {
            Source::Environment(keys) => Some(*keys),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

pub(crate) fn explain(
    field: ConfigurationField,
    revision: ConfigurationRevision,
    inputs: &ConfigurationInputs,
) -> FieldExplanation {
    let shape = DOMAINS
        .iter()
        .find(|domain| domain.field == field)
        .expect("every configuration field has a domain shape");
    let mut contributors = vec![SourceIdentity::Defaults];
    for source in shape.sources {
        match source {
            Source::Global if inputs.global.is_some() => {
                contributors.push(SourceIdentity::GlobalFile);
            }
            Source::Workspace if inputs.workspace.is_some() => {
                contributors.push(SourceIdentity::WorkspaceFile);
            }
            Source::ProjectMcp if inputs.project.is_some() => {
                contributors.push(SourceIdentity::ProjectMcpFile);
            }
            Source::PluginMcp => {} // No plugin source is present in ConfigurationInputs.
            Source::Environment(keys) => contributors.extend(
                keys.iter()
                    .filter(|key| inputs.environment.contains_key(**key))
                    .map(|key| SourceIdentity::Environment((*key).to_owned())),
            ),
            _ => {}
        }
    }
    FieldExplanation {
        field,
        revision,
        contributors,
        rule: shape.merge_rule,
        contains_sensitive_values: shape.sensitive,
    }
}

pub(crate) struct ResolvedConfiguration {
    pub settings: PeriConfig,
    pub global_settings: PeriConfig,
    pub mcp: McpConfigFile,
    pub provider: Option<ResolvedProvider>,
    pub observability: LangfuseConfig,
    pub ui: TuiConfig,
    pub resources: ResourceConfiguration,
}

pub(crate) fn resolve(
    scope: &ConfigurationScope,
    inputs: &ConfigurationInputs,
) -> Result<ResolvedConfiguration, ConfigurationError> {
    let global = parse_document(inputs.global.as_deref(), SourceIdentity::GlobalFile)?;
    let workspace = parse_document(inputs.workspace.as_deref(), SourceIdentity::WorkspaceFile)?;
    let global_settings = parse_settings(&global, SourceIdentity::GlobalFile)?;
    let mut settings = global_settings.clone();
    if inputs.workspace.is_some() {
        let overrides = parse_settings(&workspace, SourceIdentity::WorkspaceFile)?;
        settings.config.merge_overrides(overrides.config);
    }
    let mcp = resolve_mcp(scope, inputs, &HashMap::new())?;
    let provider = provider::resolve(&settings, &inputs.environment);
    let observability = observability::resolve(&global, &inputs.environment);
    let ui = TuiConfig::from_extra(&settings.config.extra);
    let resources = resources::resolve(&global);
    Ok(ResolvedConfiguration {
        settings,
        global_settings,
        mcp,
        provider,
        observability,
        ui,
        resources,
    })
}

pub(crate) fn resolve_mcp(
    scope: &ConfigurationScope,
    inputs: &ConfigurationInputs,
    plugins: &HashMap<String, McpServerConfig>,
) -> Result<McpConfigFile, ConfigurationError> {
    let global = parse_document(inputs.global.as_deref(), SourceIdentity::GlobalFile)?;
    let project = parse_document(inputs.project.as_deref(), SourceIdentity::ProjectMcpFile)?;
    let mut global = mcp::parse_global(&global).map_err(|_| ConfigurationError::InvalidInput {
        source_identity: SourceIdentity::GlobalFile,
        domain: "MCP",
    })?;
    let mut project =
        mcp::parse_project(&project).map_err(|_| ConfigurationError::InvalidInput {
            source_identity: SourceIdentity::ProjectMcpFile,
            domain: "MCP",
        })?;
    for server in global.mcp_servers.values_mut() {
        server.source = Some(ConfigSource::Global(scope.global_settings.clone()));
    }
    for server in project.mcp_servers.values_mut() {
        server.source = Some(ConfigSource::Project(project_mcp_path(&scope.cwd)));
    }
    mcp::resolve_from_files(&global, &project, plugins, &inputs.environment).map_err(|_| {
        ConfigurationError::InvalidInput {
            source_identity: if plugins.is_empty() {
                SourceIdentity::Environment(mcp::MCP_CACHE_ENV.to_owned())
            } else {
                SourceIdentity::Defaults
            },
            domain: if plugins.is_empty() {
                "MCP cache"
            } else {
                "MCP plugin inputs"
            },
        }
    })
}

pub(crate) fn parse_document(
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

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
