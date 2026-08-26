use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{dynamic_mcp::SessionMcpCapabilitySnapshot, ports::SessionMcpCapabilityPort};

use crate::tools::{BaseTool, ToolDefinition};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogRefreshError {
    #[error("dynamic MCP capability snapshot is inconsistent")]
    InconsistentCapability,
    #[error("tool alias conflicts with another visible tool")]
    AliasConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    CoreOrMiddleware,
    StaticMcp(String),
    DynamicMcp(peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey),
}

#[derive(Clone)]
pub struct CatalogToolEntry {
    pub tool: Arc<dyn BaseTool>,
    pub source: ToolSource,
}

#[derive(Clone, Default)]
pub struct SessionToolCatalogSnapshot {
    pub generation: u64,
    pub tools: BTreeMap<String, CatalogToolEntry>,
    pub direct_definitions: Vec<ToolDefinition>,
    pub aliases: BTreeMap<String, String>,
}

impl std::fmt::Debug for SessionToolCatalogSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionToolCatalogSnapshot")
            .field("generation", &self.generation)
            .field("tool_names", &self.tools.keys().collect::<Vec<_>>())
            .field("direct_definitions", &self.direct_definitions)
            .field("aliases", &self.aliases)
            .finish()
    }
}

impl SessionToolCatalogSnapshot {
    pub fn tool_map(&self) -> BTreeMap<String, Arc<dyn BaseTool>> {
        self.tools
            .iter()
            .map(|(name, entry)| (name.clone(), Arc::clone(&entry.tool)))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolFilterPolicy {
    InheritAll,
    AllowNone,
    AllowList(Vec<String>),
}

impl ToolFilterPolicy {
    pub fn canonical(
        allowed: Option<Vec<String>>,
        disallowed: Vec<String>,
    ) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
        let policy = match allowed {
            None => Self::InheritAll,
            Some(allowed) if allowed.is_empty() => Self::AllowNone,
            Some(allowed) => Self::AllowList(
                allowed
                    .into_iter()
                    .map(|name| name.to_lowercase())
                    .collect(),
            ),
        };
        let disallowed = disallowed
            .into_iter()
            .map(|name| name.to_lowercase())
            .collect::<Vec<_>>();
        Arc::new(move |name| {
            let name = name.to_lowercase();
            let allowed = match &policy {
                ToolFilterPolicy::InheritAll => true,
                ToolFilterPolicy::AllowNone => false,
                ToolFilterPolicy::AllowList(names) => names
                    .iter()
                    .any(|candidate| candidate == "*" || candidate == &name),
            };
            allowed && !disallowed.iter().any(|candidate| candidate == &name)
        })
    }
}

pub struct SessionToolCatalog {
    base_tools: BTreeMap<String, CatalogToolEntry>,
    published: RwLock<Arc<SessionToolCatalogSnapshot>>,
    capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl SessionToolCatalog {
    pub fn new(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    ) -> Self {
        Self::with_filter(base_tools, capability, Arc::new(|_| true))
    }

    pub fn with_filter(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
        tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        let base_tools = base_tools
            .into_iter()
            .map(|(name, tool)| {
                let source = tool
                    .mcp_server_name()
                    .map(str::to_owned)
                    .or_else(|| static_mcp_server(&name))
                    .map(ToolSource::StaticMcp)
                    .unwrap_or(ToolSource::CoreOrMiddleware);
                (name, CatalogToolEntry { tool, source })
            })
            .collect::<BTreeMap<_, _>>();
        let initial = Arc::new(
            build_snapshot(0, &base_tools, None)
                .expect("base tool catalog must not contain conflicting aliases"),
        );
        Self {
            base_tools,
            published: RwLock::new(initial),
            capability,
            tool_filter,
        }
    }

    pub fn dynamic_catalog_tools(&self) -> Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool> {
        self.base_tools
            .iter()
            .map(
                |(name, entry)| peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool {
                    name: name.clone(),
                    aliases: entry
                        .tool
                        .aliases()
                        .iter()
                        .map(|alias| (*alias).to_string())
                        .collect(),
                    static_mcp_server: match &entry.source {
                        ToolSource::StaticMcp(server) => Some(server.clone()),
                        ToolSource::CoreOrMiddleware | ToolSource::DynamicMcp(_) => None,
                    },
                },
            )
            .collect()
    }

    pub fn snapshot(&self) -> Arc<SessionToolCatalogSnapshot> {
        Arc::clone(&self.published.read())
    }

    pub fn refresh(&self) -> Result<Arc<SessionToolCatalogSnapshot>, CatalogRefreshError> {
        let capability = self
            .capability
            .as_ref()
            .map(|port| port.snapshot())
            .unwrap_or_default();
        let current = self.snapshot();
        if current.generation == capability.generation {
            return Ok(current);
        }
        let mut next_tools = build_tools(&self.base_tools, Some(&capability))?;
        next_tools.retain(|name, _| (self.tool_filter)(name));
        let next = Arc::new(finalize(capability.generation, next_tools)?);
        *self.published.write() = Arc::clone(&next);
        Ok(next)
    }

    /// Pin the exact request-local working tool objects after middleware has
    /// rebound meta tools. The returned snapshot belongs only to this Reason;
    /// request-local bindings must never replace the session publisher.
    pub fn pin_working_tools(
        &self,
        working: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> Result<Arc<SessionToolCatalogSnapshot>, CatalogRefreshError> {
        let current = self.snapshot();
        let tools = working
            .iter()
            .map(|(name, tool)| {
                let source = current
                    .tools
                    .get(name)
                    .map(|entry| entry.source.clone())
                    .unwrap_or(ToolSource::CoreOrMiddleware);
                (
                    name.clone(),
                    CatalogToolEntry {
                        tool: Arc::clone(tool),
                        source,
                    },
                )
            })
            .collect();
        finalize(current.generation, tools).map(Arc::new)
    }
}

fn build_snapshot(
    generation: u64,
    base: &BTreeMap<String, CatalogToolEntry>,
    capability: Option<&SessionMcpCapabilitySnapshot>,
) -> Result<SessionToolCatalogSnapshot, CatalogRefreshError> {
    finalize(generation, build_tools(base, capability)?)
}

fn build_tools(
    base: &BTreeMap<String, CatalogToolEntry>,
    capability: Option<&SessionMcpCapabilitySnapshot>,
) -> Result<BTreeMap<String, CatalogToolEntry>, CatalogRefreshError> {
    let mut tools = base.clone();
    if let Some(capability) = capability {
        for server in capability.servers.keys() {
            tools.retain(|_, entry| {
                !matches!(&entry.source, ToolSource::StaticMcp(source) if source == server)
            });
        }
        for (name, dynamic_tool) in &capability.tools {
            let Some(projection) = capability
                .servers
                .get(&dynamic_tool.instance.logical.server_name)
            else {
                return Err(CatalogRefreshError::InconsistentCapability);
            };
            if projection.instance_key != dynamic_tool.instance {
                return Err(CatalogRefreshError::InconsistentCapability);
            }
            tools.insert(
                name.clone(),
                CatalogToolEntry {
                    tool: Arc::clone(&dynamic_tool.tool),
                    source: ToolSource::DynamicMcp(dynamic_tool.instance.clone()),
                },
            );
        }
    }
    Ok(tools)
}

fn finalize(
    generation: u64,
    tools: BTreeMap<String, CatalogToolEntry>,
) -> Result<SessionToolCatalogSnapshot, CatalogRefreshError> {
    let direct_definitions = tools
        .values()
        .filter(|entry| entry.tool.is_direct())
        .map(|entry| entry.tool.definition())
        .collect();
    let mut aliases = BTreeMap::new();
    for (name, entry) in &tools {
        for alias in entry.tool.aliases() {
            let alias = alias.to_ascii_lowercase();
            if let Some(existing) = aliases.insert(alias, name.clone()) {
                if existing != *name {
                    return Err(CatalogRefreshError::AliasConflict);
                }
            }
        }
    }
    Ok(SessionToolCatalogSnapshot {
        generation,
        tools,
        direct_definitions,
        aliases,
    })
}

fn static_mcp_server(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, _) = rest.split_once("__")?;
    Some(server.to_string())
}

#[cfg(test)]
#[path = "tool_catalog_test.rs"]
mod tests;
