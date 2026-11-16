//! v5 builtin capability selection for `peri mcp-start` and `.mcp.json`.
//!
//! Capability names mirror `peri-acp-types::builtin_mcp::BUILTIN_V5_CAPABILITIES`
//! (the declaration authority); the local list exists only for the typed parser,
//! and a unit test locks both lists together. This module only parses and
//! resolves selection so configuration errors are typed and carry their source;
//! `peri-middlewares::mcp::builtin::router` serves the resolved capability set.
//!
//! The `.mcp.json` extension key is `mcpServers.peri.x-peri.capabilities`.
//! `x-peri` is provisional: the design (#416) leaves the key name undecided
//! until an MCP ecosystem review. Do not add a top-level `capabilities` object:
//! MCP already uses that name for protocol negotiation.
//!
//! Standalone never reads `settings.json`. Legacy `meta_harness` policy keys
//! (for example `WebMiddleware`) were retired at M4: they are not a second
//! capability switch. The remaining compatibility adapter is the legacy
//! per-instance `mcpServers.<instance>` entry mapped to one capability.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;
use thiserror::Error;

pub const BUILTIN_CAPABILITY_NAMES: &[&str] = &[
    "workspace",
    "web",
    "artifact",
    "cron",
    "monitor",
    "workflow",
];

/// Server entry reserved for the single builtin exit and for standalone clients.
pub const PERI_SERVER_NAME: &str = "peri";

/// Provisional namespace extension on the `peri` server entry.
pub const PERI_EXTENSION_KEY: &str = "x-peri";

/// Capability list inside [`PERI_EXTENSION_KEY`].
pub const CAPABILITIES_FIELD: &str = "capabilities";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinCapability {
    Workspace,
    Web,
    Artifact,
    Cron,
    Monitor,
    Workflow,
}

impl BuiltinCapability {
    pub const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Web,
        Self::Artifact,
        Self::Cron,
        Self::Monitor,
        Self::Workflow,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Web => "web",
            Self::Artifact => "artifact",
            Self::Cron => "cron",
            Self::Monitor => "monitor",
            Self::Workflow => "workflow",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.name() == name)
    }

    /// Legacy per-instance `.mcp.json` key; separate from the v5 model name
    /// because the router may later rename the exposed server.
    pub fn legacy_instance_name(self) -> &'static str {
        self.name()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityRequest {
    All,
    None,
    Names(BTreeSet<BuiltinCapability>),
}

impl CapabilityRequest {
    /// Expanded set; `None` means the selector is explicitly empty.
    pub fn selected(&self) -> Option<BTreeSet<BuiltinCapability>> {
        match self {
            Self::All => Some(BuiltinCapability::ALL.into_iter().collect()),
            Self::None => None,
            Self::Names(names) => Some(names.clone()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::None => "none".to_string(),
            Self::Names(names) => names
                .iter()
                .map(|capability| capability.name())
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    /// `peri mcp-start --capability`.
    Cli,
    /// `mcpServers.peri.x-peri.capabilities`.
    PeriExtension,
    /// Legacy `mcpServers.<instance>` entry.
    LegacyInstanceEntry,
    /// Builtin default (all capabilities enabled).
    Default,
}

impl fmt::Display for CapabilitySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => formatter.write_str("command line (--capability)"),
            Self::PeriExtension => formatter.write_str("mcpServers.peri.x-peri.capabilities"),
            Self::LegacyInstanceEntry => formatter.write_str("legacy mcpServers.<instance> entry"),
            Self::Default => formatter.write_str("builtin default"),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CapabilityError {
    #[error("MCP capability configuration must be a JSON object")]
    InvalidDocument,
    #[error("mcpServers must be a JSON object")]
    InvalidMcpServers,
    #[error("mcpServers.peri must be a JSON object")]
    InvalidPeriEntry,
    #[error(
        "{location} is not a Peri capability selector; use mcpServers.peri.x-peri.capabilities"
    )]
    MisplacedCapabilities { location: &'static str },
    #[error("mcpServers.peri.x-peri must be a JSON object")]
    InvalidExtension,
    #[error("mcpServers.peri.x-peri.capabilities must be an array of capability names")]
    InvalidCapabilitiesValue,
    #[error("mcpServers.peri.x-peri.capabilities[{index}] must be a string")]
    InvalidCapabilityItem { index: usize },
    #[error("legacy mcpServers.{name} entry must be a JSON object")]
    InvalidLegacyInstance { name: String },
    #[error("legacy mcpServers.{name}.disabled must be a boolean")]
    InvalidLegacyDisabled { name: String },
    #[error(
        "legacy mcpServers.{name} must not declare command/url: builtin instance names are reserved"
    )]
    ReservedInstanceOverride { name: String },
    #[error(
        "legacy capability switches disagree for `{capability}` ({first} vs {second}); \
         remove one legacy mechanism"
    )]
    LegacyConflict {
        capability: &'static str,
        first: CapabilitySource,
        second: CapabilitySource,
    },
    #[error(
        "unknown builtin MCP capability: `{name}` \
         (expected one of: workspace, web, artifact, cron, monitor, workflow; \
         origin: {origin})"
    )]
    UnknownCapability {
        name: String,
        origin: CapabilitySource,
    },
    #[error("empty capability name from {origin}")]
    EmptyCapabilityToken { origin: CapabilitySource },
    #[error(
        "capability selection from {origin} is empty; at least one capability must be enabled"
    )]
    EmptySelection { origin: CapabilitySource },
    #[error("duplicate capability `{name}` from {origin}")]
    DuplicateCapability {
        name: String,
        origin: CapabilitySource,
    },
    #[error("duplicate selector `{selector}` from {origin}")]
    DuplicateSelector {
        selector: &'static str,
        origin: CapabilitySource,
    },
    #[error("`all` cannot be combined with capability names (origin: {origin})")]
    AllWithNames { origin: CapabilitySource },
    #[error("`none` cannot be combined with capability names (origin: {origin})")]
    NoneWithNames { origin: CapabilitySource },
    #[error("`all` and `none` cannot be combined (origin: {origin})")]
    AllAndNone { origin: CapabilitySource },
}

/// Parse repeatable and comma-separated capability tokens.
///
/// `all` and `none` are exclusive selectors; duplicates and empty segments are
/// load-time errors instead of being folded away.
pub fn parse_capability_tokens<I, S>(
    tokens: I,
    source: CapabilitySource,
) -> Result<CapabilityRequest, CapabilityError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut names = BTreeSet::new();
    let mut selector: Option<&'static str> = None;
    let mut saw_token = false;

    for token in tokens {
        saw_token = true;
        for part in token.as_ref().split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(CapabilityError::EmptyCapabilityToken { origin: source });
            }
            match part {
                "all" => {
                    if selector == Some("none") {
                        return Err(CapabilityError::AllAndNone { origin: source });
                    }
                    if selector == Some("all") {
                        return Err(CapabilityError::DuplicateSelector {
                            selector: "all",
                            origin: source,
                        });
                    }
                    if !names.is_empty() {
                        return Err(CapabilityError::AllWithNames { origin: source });
                    }
                    selector = Some("all");
                }
                "none" => {
                    if selector == Some("all") {
                        return Err(CapabilityError::AllAndNone { origin: source });
                    }
                    if selector == Some("none") {
                        return Err(CapabilityError::DuplicateSelector {
                            selector: "none",
                            origin: source,
                        });
                    }
                    if !names.is_empty() {
                        return Err(CapabilityError::NoneWithNames { origin: source });
                    }
                    selector = Some("none");
                }
                name => {
                    if selector == Some("all") {
                        return Err(CapabilityError::AllWithNames { origin: source });
                    }
                    if selector == Some("none") {
                        return Err(CapabilityError::NoneWithNames { origin: source });
                    }
                    let capability = BuiltinCapability::from_name(name).ok_or_else(|| {
                        CapabilityError::UnknownCapability {
                            name: name.to_string(),
                            origin: source,
                        }
                    })?;
                    if !names.insert(capability) {
                        return Err(CapabilityError::DuplicateCapability {
                            name: name.to_string(),
                            origin: source,
                        });
                    }
                }
            }
        }
    }

    if !saw_token {
        return Err(CapabilityError::EmptySelection { origin: source });
    }
    Ok(match selector {
        Some("all") => CapabilityRequest::All,
        Some("none") => CapabilityRequest::None,
        _ if names.is_empty() => return Err(CapabilityError::EmptySelection { origin: source }),
        _ => CapabilityRequest::Names(names),
    })
}

/// Capability switches found in one `.mcp.json` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpCapabilityFile {
    extension: Option<CapabilityRequest>,
    legacy_instances: BTreeMap<BuiltinCapability, bool>,
}

impl McpCapabilityFile {
    pub fn extension(&self) -> Option<&CapabilityRequest> {
        self.extension.as_ref()
    }

    pub fn legacy_instances(&self) -> &BTreeMap<BuiltinCapability, bool> {
        &self.legacy_instances
    }

    pub fn is_empty(&self) -> bool {
        self.extension.is_none() && self.legacy_instances.is_empty()
    }
}

/// Read capability switches from a raw `.mcp.json` document.
///
/// Standard server fields are ignored. Legacy per-instance entries map to
/// capability switches; the retired `meta_harness` policy keys are not a second
/// switch. Unknown `*Middleware` keys in that misplaced section are ignored by
/// this standalone parser (the host settings loader warns and drops them).
pub fn parse_mcp_file_capabilities(document: &Value) -> Result<McpCapabilityFile, CapabilityError> {
    let object = document
        .as_object()
        .ok_or(CapabilityError::InvalidDocument)?;
    if object.contains_key(CAPABILITIES_FIELD) {
        return Err(CapabilityError::MisplacedCapabilities {
            location: "top-level capabilities",
        });
    }

    let servers = match object.get("mcpServers") {
        None => None,
        Some(Value::Object(servers)) => Some(servers),
        Some(_) => return Err(CapabilityError::InvalidMcpServers),
    };

    let mut extension = None;
    let mut legacy_instances = BTreeMap::new();
    if let Some(servers) = servers {
        if let Some(entry) = servers.get(PERI_SERVER_NAME) {
            extension = parse_peri_extension(entry)?;
        }
        for capability in BuiltinCapability::ALL {
            let name = capability.legacy_instance_name();
            if let Some(entry) = servers.get(name) {
                let enabled = parse_legacy_instance(name, entry)?;
                legacy_instances.insert(capability, enabled);
            }
        }
    }

    Ok(McpCapabilityFile {
        extension,
        legacy_instances,
    })
}

fn parse_peri_extension(entry: &Value) -> Result<Option<CapabilityRequest>, CapabilityError> {
    let object = entry.as_object().ok_or(CapabilityError::InvalidPeriEntry)?;
    if object.contains_key(CAPABILITIES_FIELD) {
        return Err(CapabilityError::MisplacedCapabilities {
            location: "mcpServers.peri.capabilities",
        });
    }
    let Some(extension) = object.get(PERI_EXTENSION_KEY) else {
        return Ok(None);
    };
    let extension = extension
        .as_object()
        .ok_or(CapabilityError::InvalidExtension)?;
    let Some(capabilities) = extension.get(CAPABILITIES_FIELD) else {
        return Ok(None);
    };
    let capabilities = capabilities
        .as_array()
        .ok_or(CapabilityError::InvalidCapabilitiesValue)?;
    let tokens = capabilities
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .ok_or(CapabilityError::InvalidCapabilityItem { index })
        })
        .collect::<Result<Vec<_>, _>>()?;
    parse_capability_tokens(tokens, CapabilitySource::PeriExtension).map(Some)
}

fn parse_legacy_instance(name: &str, entry: &Value) -> Result<bool, CapabilityError> {
    let object = entry
        .as_object()
        .ok_or_else(|| CapabilityError::InvalidLegacyInstance {
            name: name.to_string(),
        })?;
    if object.contains_key("command") || object.contains_key("url") {
        return Err(CapabilityError::ReservedInstanceOverride {
            name: name.to_string(),
        });
    }
    match object.get("disabled") {
        None => Ok(true),
        Some(Value::Bool(disabled)) => Ok(!disabled),
        Some(_) => Err(CapabilityError::InvalidLegacyDisabled {
            name: name.to_string(),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDecision {
    pub enabled: bool,
    pub source: CapabilitySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityConflict {
    pub capability: BuiltinCapability,
    pub winner: CapabilitySource,
    pub overridden: CapabilitySource,
    pub winner_enabled: bool,
    pub overridden_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCapabilities {
    decisions: BTreeMap<BuiltinCapability, CapabilityDecision>,
    conflicts: Vec<CapabilityConflict>,
}

impl ResolvedCapabilities {
    pub fn decision(&self, capability: BuiltinCapability) -> CapabilityDecision {
        self.decisions[&capability]
    }

    pub fn is_enabled(&self, capability: BuiltinCapability) -> bool {
        self.decision(capability).enabled
    }

    pub fn enabled(&self) -> BTreeSet<BuiltinCapability> {
        self.decisions
            .iter()
            .filter(|(_, decision)| decision.enabled)
            .map(|(capability, _)| *capability)
            .collect()
    }

    pub fn enabled_names(&self) -> Vec<&'static str> {
        self.enabled()
            .into_iter()
            .map(BuiltinCapability::name)
            .collect()
    }

    pub fn conflicts(&self) -> &[CapabilityConflict] {
        &self.conflicts
    }
}

/// Resolve the effective capability set with `CLI > .mcp.json > default`.
///
/// A selector replaces the whole set, so conflicting explicit sources are
/// visible in [`ResolvedCapabilities::conflicts`] instead of being merged.
pub fn resolve_capabilities(
    cli: Option<&CapabilityRequest>,
    file: Option<&McpCapabilityFile>,
) -> Result<ResolvedCapabilities, CapabilityError> {
    let mut decisions: BTreeMap<_, _> = BuiltinCapability::ALL
        .into_iter()
        .map(|capability| {
            (
                capability,
                CapabilityDecision {
                    enabled: true,
                    source: CapabilitySource::Default,
                },
            )
        })
        .collect();
    let mut conflicts = Vec::new();
    let mut explicit: BTreeMap<BuiltinCapability, (bool, CapabilitySource)> = BTreeMap::new();

    if let Some(file) = file {
        for (capability, enabled) in &file.legacy_instances {
            if let Some((previous, previous_source)) = explicit.get(capability) {
                if *previous != *enabled {
                    return Err(CapabilityError::LegacyConflict {
                        capability: capability.name(),
                        first: *previous_source,
                        second: CapabilitySource::LegacyInstanceEntry,
                    });
                }
            }
            explicit.insert(
                *capability,
                (*enabled, CapabilitySource::LegacyInstanceEntry),
            );
            decisions.insert(
                *capability,
                CapabilityDecision {
                    enabled: *enabled,
                    source: CapabilitySource::LegacyInstanceEntry,
                },
            );
        }
        if let Some(request) = &file.extension {
            apply_selector(
                &mut decisions,
                &mut conflicts,
                request.selected().as_ref(),
                CapabilitySource::PeriExtension,
            );
        }
    }

    if let Some(request) = cli {
        apply_selector(
            &mut decisions,
            &mut conflicts,
            request.selected().as_ref(),
            CapabilitySource::Cli,
        );
    }

    if decisions.values().all(|decision| !decision.enabled) {
        let source = decisions
            .values()
            .find(|decision| decision.source != CapabilitySource::Default)
            .map(|decision| decision.source)
            .or_else(|| {
                file.and_then(|file| {
                    file.legacy_instances
                        .keys()
                        .next()
                        .map(|_| CapabilitySource::LegacyInstanceEntry)
                })
            })
            .unwrap_or(CapabilitySource::Default);
        return Err(CapabilityError::EmptySelection { origin: source });
    }

    Ok(ResolvedCapabilities {
        decisions,
        conflicts,
    })
}

fn apply_selector(
    decisions: &mut BTreeMap<BuiltinCapability, CapabilityDecision>,
    conflicts: &mut Vec<CapabilityConflict>,
    selected: Option<&BTreeSet<BuiltinCapability>>,
    source: CapabilitySource,
) {
    for capability in BuiltinCapability::ALL {
        let previous = decisions[&capability];
        let enabled = selected.is_some_and(|selected| selected.contains(&capability));
        if previous.source != CapabilitySource::Default && previous.enabled != enabled {
            conflicts.push(CapabilityConflict {
                capability,
                winner: source,
                overridden: previous.source,
                winner_enabled: enabled,
                overridden_enabled: previous.enabled,
            });
        }
        decisions.insert(capability, CapabilityDecision { enabled, source });
    }
}

#[cfg(test)]
#[path = "mcp_capabilities_test.rs"]
mod tests;
