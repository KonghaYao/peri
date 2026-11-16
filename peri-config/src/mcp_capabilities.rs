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
//! Standalone never reads `settings.json`. A `meta_harness` object found in
//! `.mcp.json` is accepted only as a v4 compatibility bridge: known policy keys
//! map to capabilities and unknown `*Middleware` keys are typed errors.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};
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

/// Legacy settings section that held per-instance policy keys.
const LEGACY_POLICY_MAP: &str = "meta_harness";

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

    /// Legacy `meta_harness` policy key that used to switch this instance.
    pub fn legacy_policy_key(self) -> &'static str {
        match self {
            Self::Workspace => "WorkspaceMiddleware",
            Self::Web => "WebMiddleware",
            Self::Artifact => "ArtifactMiddleware",
            Self::Cron => "CronMiddleware",
            Self::Monitor => "MonitorMiddleware",
            Self::Workflow => "WorkflowMiddleware",
        }
    }

    /// Legacy per-instance `.mcp.json` key; separate from the v5 model name
    /// because the router may later rename the exposed server.
    pub fn legacy_instance_name(self) -> &'static str {
        self.name()
    }
}

pub fn capability_from_legacy_policy_key(key: &str) -> Option<BuiltinCapability> {
    BuiltinCapability::ALL
        .into_iter()
        .find(|capability| capability.legacy_policy_key() == key)
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
    /// Legacy `meta_harness` policy key.
    LegacyPolicyKey,
    /// Builtin default (all capabilities enabled).
    Default,
}

impl fmt::Display for CapabilitySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => formatter.write_str("command line (--capability)"),
            Self::PeriExtension => formatter.write_str("mcpServers.peri.x-peri.capabilities"),
            Self::LegacyInstanceEntry => formatter.write_str("legacy mcpServers.<instance> entry"),
            Self::LegacyPolicyKey => formatter.write_str("legacy meta_harness policy key"),
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
    #[error("meta_harness must be a JSON object")]
    InvalidLegacyPolicyMap,
    #[error("legacy meta_harness key `{key}` must be a boolean")]
    InvalidLegacyPolicyValue { key: String },
    #[error(
        "unknown legacy meta_harness policy key `{key}` (expected one of: \
         WorkspaceMiddleware, WebMiddleware, ArtifactMiddleware, CronMiddleware, \
         MonitorMiddleware, WorkflowMiddleware); \
         map it to a capability or remove it, it is not silently ignored"
    )]
    UnknownLegacyPolicyKey { key: String },
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
    legacy_policy: BTreeMap<BuiltinCapability, bool>,
}

impl McpCapabilityFile {
    pub fn extension(&self) -> Option<&CapabilityRequest> {
        self.extension.as_ref()
    }

    pub fn legacy_instances(&self) -> &BTreeMap<BuiltinCapability, bool> {
        &self.legacy_instances
    }

    pub fn legacy_policy(&self) -> &BTreeMap<BuiltinCapability, bool> {
        &self.legacy_policy
    }

    pub fn is_empty(&self) -> bool {
        self.extension.is_none()
            && self.legacy_instances.is_empty()
            && self.legacy_policy.is_empty()
    }
}

/// Read capability switches from a raw `.mcp.json` document.
///
/// Standard server fields are ignored. Legacy per-instance entries map to
/// capability switches; legacy policy keys in a misplaced `meta_harness`
/// section map the same way. Unknown `*Middleware` keys are typed errors so an
/// old key can never survive as a silent no-op.
pub fn parse_mcp_file_capabilities(document: &Value) -> Result<McpCapabilityFile, CapabilityError> {
    let object = document
        .as_object()
        .ok_or(CapabilityError::InvalidDocument)?;
    if object.contains_key(CAPABILITIES_FIELD) {
        return Err(CapabilityError::MisplacedCapabilities {
            location: "top-level capabilities",
        });
    }

    let legacy_policy = match object.get(LEGACY_POLICY_MAP) {
        None => BTreeMap::new(),
        Some(Value::Object(map)) => parse_legacy_policy_map(map)?,
        Some(_) => return Err(CapabilityError::InvalidLegacyPolicyMap),
    };

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
        legacy_policy,
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

fn parse_legacy_policy_map(
    map: &Map<String, Value>,
) -> Result<BTreeMap<BuiltinCapability, bool>, CapabilityError> {
    let mut policy = BTreeMap::new();
    for (key, value) in map {
        let Some(capability) = capability_from_legacy_policy_key(key) else {
            if key.ends_with("Middleware") {
                return Err(CapabilityError::UnknownLegacyPolicyKey { key: key.clone() });
            }
            continue;
        };
        let enabled = value
            .as_bool()
            .ok_or_else(|| CapabilityError::InvalidLegacyPolicyValue { key: key.clone() })?;
        policy.insert(capability, enabled);
    }
    Ok(policy)
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
        for (capability, enabled) in &file.legacy_policy {
            explicit.insert(*capability, (*enabled, CapabilitySource::LegacyPolicyKey));
            decisions.insert(
                *capability,
                CapabilityDecision {
                    enabled: *enabled,
                    source: CapabilitySource::LegacyPolicyKey,
                },
            );
        }
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
                    file.legacy_policy
                        .keys()
                        .next()
                        .map(|_| CapabilitySource::LegacyPolicyKey)
                        .or_else(|| {
                            file.legacy_instances
                                .keys()
                                .next()
                                .map(|_| CapabilitySource::LegacyInstanceEntry)
                        })
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
