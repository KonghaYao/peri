use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{
    dynamic_mcp::{DynamicMcpCatalogTool, SessionMcpCapabilitySnapshot},
    ports::SessionMcpCapabilityPort,
};

use crate::tools::{BaseTool, ToolDefinition};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogRefreshError {
    #[error("dynamic MCP capability snapshot is inconsistent")]
    InconsistentCapability,
    #[error("tool alias conflicts with another visible tool")]
    AliasConflict,
    /// 启动候选工具不是静态 MCP 身份（名称非 `mcp__{server}__{tool}`）。
    #[error("startup tool `{tool}` is not a static MCP tool name")]
    InvalidStartupSource { tool: String },
    /// 启动候选要覆盖 core/middleware 或其它 server 的静态条目。
    #[error("startup tool `{tool}` collides with a non-replaceable catalog entry")]
    StartupRegistrationRejected { tool: String },
    /// 启动候选提交后必需工具仍不可直接使用（被策略过滤或动态遮蔽）。
    #[error("required startup tool `{tool}` of MCP server `{server}` is unavailable")]
    RequiredToolUnavailable { server: String, tool: String },
}

/// 启动闸门提交的必需工具身份（跨层 seam：由 MCP middleware 按 server 归属上报）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRequiredTool {
    pub server_name: String,
    pub original_tool_name: String,
    pub effective_tool_name: String,
}

/// 启动闸门候选：整批静态 MCP 工具（required 已提升 direct）+ 必需工具身份。
///
/// 由 middleware 在 `before_react_start` 中经 `StartupState` 暂存，失败的整批
/// 直接随 state 丢弃，不落 middleware 内部字段、不发布部分结果。
#[derive(Clone)]
pub struct StartupToolUpdate {
    pub tools: Vec<Arc<dyn BaseTool>>,
    pub required: Vec<StartupRequiredTool>,
}

impl std::fmt::Debug for StartupToolUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartupToolUpdate")
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .field("required", &self.required)
            .finish()
    }
}

/// 动态碰撞目录的同步注册回调：静态 base 提交成功后按新目录重验动态注册。
///
/// 锁序为 catalog → registration → registry；回调**不得**重入 catalog。
pub(crate) type StartupCatalogRegistration =
    Arc<dyn Fn(Vec<DynamicMcpCatalogTool>) -> Result<(), CatalogRefreshError> + Send + Sync>;

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

/// 静态事实（base）与对外快照（published）在同一把锁内提交。
struct CatalogState {
    base_tools: BTreeMap<String, CatalogToolEntry>,
    published: Arc<SessionToolCatalogSnapshot>,
}

pub struct SessionToolCatalog {
    state: RwLock<CatalogState>,
    capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// 动态碰撞目录注册回调；缺省表示本次会话未接入碰撞目录（子 agent 等）。
    startup_registration: RwLock<Option<StartupCatalogRegistration>>,
}

impl SessionToolCatalog {
    pub fn new(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    ) -> Self {
        Self::try_new(base_tools, capability)
            .expect("base tool catalog must not contain conflicting aliases")
    }

    pub fn try_new(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
    ) -> Result<Self, CatalogRefreshError> {
        Self::try_with_filter(base_tools, capability, Arc::new(|_| true))
    }

    pub fn with_filter(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
        tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        Self::try_with_filter(base_tools, capability, tool_filter)
            .expect("base tool catalog must not contain conflicting aliases")
    }

    pub fn try_with_filter(
        base_tools: BTreeMap<String, Arc<dyn BaseTool>>,
        capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
        tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Result<Self, CatalogRefreshError> {
        let base_tools = base_tools
            .into_iter()
            .map(|(name, tool)| {
                let source = base_entry_source(tool.as_ref(), &name);
                (name, CatalogToolEntry { tool, source })
            })
            .collect::<BTreeMap<_, _>>();
        let initial = Arc::new(build_snapshot(0, &base_tools, None)?);
        Ok(Self {
            state: RwLock::new(CatalogState {
                base_tools,
                published: initial,
            }),
            capability,
            tool_filter,
            startup_registration: RwLock::new(None),
        })
    }

    /// 注入动态碰撞目录的同步注册回调（静态 base 提交成功后按新目录重验）。
    ///
    /// 调用点归 `session/exec/stage_builder/tools.rs`（捕获 deployment 与
    /// session_id）；未接线的目录（子 agent 沿用自身 capability）保持 None。
    pub(crate) fn set_startup_catalog_registration(
        &self,
        registration: StartupCatalogRegistration,
    ) {
        *self.startup_registration.write() = Some(registration);
    }

    pub fn dynamic_catalog_tools(&self) -> Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool> {
        dynamic_catalog_tools_of(&self.state.read().base_tools)
    }

    pub fn snapshot(&self) -> Arc<SessionToolCatalogSnapshot> {
        Arc::clone(&self.state.read().published)
    }

    pub fn refresh(&self) -> Result<Arc<SessionToolCatalogSnapshot>, CatalogRefreshError> {
        let capability = self.capability_snapshot();
        let mut state = self.state.write();
        if state.published.generation == capability.generation {
            return Ok(Arc::clone(&state.published));
        }
        let next = Arc::new(build_published(
            capability.generation,
            &state.base_tools,
            Some(&capability),
            self.tool_filter.as_ref(),
        )?);
        state.published = Arc::clone(&next);
        Ok(next)
    }

    /// 把启动闸门准入的整批静态 MCP bridge 原子提交到 static base。
    ///
    /// 只更新 base：Reason 边界仍完整走 ARC-TOOLS-001 的
    /// `refresh → working map swap → before_reason_catalog → before_model → pin`，
    /// 本提交不替代 Reason boundary，也不混入 dynamic overlay（overlay 由
    /// capability 快照在每次发布时重新叠加）。
    ///
    /// 同步、fallible、先验证后提交：来源、与既有 base 条目的身份冲突、别名
    /// 冲突、必需工具在发布快照中可直达（`is_direct && visible_to_model` 且未被
    /// `tool_filter`/动态遮蔽吞掉）全部通过后才替换 base 与 published。
    pub(crate) fn replace_static_mcp_tools(
        &self,
        update: StartupToolUpdate,
    ) -> Result<Arc<SessionToolCatalogSnapshot>, CatalogRefreshError> {
        let capability = self.capability_snapshot();
        let mut state = self.state.write();
        let mut next_base = state.base_tools.clone();
        for tool in update.tools {
            let name = tool.name().to_string();
            let entry = startup_entry(&name, tool)?;
            let collides = next_base
                .get(&name)
                .is_some_and(|existing| existing.source != entry.source);
            if collides {
                return Err(CatalogRefreshError::StartupRegistrationRejected { tool: name });
            }
            next_base.insert(name, entry);
        }
        let next = Arc::new(build_published(
            capability.generation,
            &next_base,
            Some(&capability),
            self.tool_filter.as_ref(),
        )?);
        for required in &update.required {
            let reachable = next
                .direct_definitions
                .iter()
                .any(|definition| definition.name == required.effective_tool_name);
            if !reachable {
                return Err(CatalogRefreshError::RequiredToolUnavailable {
                    server: required.server_name.clone(),
                    tool: required.effective_tool_name.clone(),
                });
            }
        }
        if let Some(registration) = self.startup_registration.read().clone() {
            // 锁序 catalog → registration → registry；回调不得重入 catalog。
            registration(dynamic_catalog_tools_of(&next_base))?;
        }
        state.base_tools = next_base;
        state.published = Arc::clone(&next);
        Ok(next)
    }

    fn capability_snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
        self.capability
            .as_ref()
            .map(|port| port.snapshot())
            .unwrap_or_default()
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

/// 一次发布的完整构造：静态 base → dynamic overlay → session tool_filter。
///
/// 单一入口保证 initial / refresh / startup 提交三条路径的可见性语义一致。
fn build_published(
    generation: u64,
    base: &BTreeMap<String, CatalogToolEntry>,
    capability: Option<&SessionMcpCapabilitySnapshot>,
    tool_filter: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<SessionToolCatalogSnapshot, CatalogRefreshError> {
    let mut tools = build_tools(base, capability)?;
    tools.retain(|name, _| tool_filter(name));
    finalize(generation, tools)
}

/// 既有 base 条目的来源归属：工具自声明优先，其次按 `mcp__{server}__{tool}` 形态推导。
fn base_entry_source(tool: &dyn BaseTool, name: &str) -> ToolSource {
    tool.mcp_server_name()
        .map(str::to_owned)
        .or_else(|| static_mcp_server(name))
        .map(ToolSource::StaticMcp)
        .unwrap_or(ToolSource::CoreOrMiddleware)
}

/// 启动候选条目的来源归属：名称必须已是静态 MCP 有效名，凭此拒绝借启动闸门
/// 注入 core/middleware 身份或覆盖其它 server 的条目。
fn startup_entry(
    name: &str,
    tool: Arc<dyn BaseTool>,
) -> Result<CatalogToolEntry, CatalogRefreshError> {
    let Some(name_server) = static_mcp_server(name) else {
        return Err(CatalogRefreshError::InvalidStartupSource {
            tool: name.to_string(),
        });
    };
    let server = tool
        .mcp_server_name()
        .map(str::to_owned)
        .unwrap_or(name_server);
    Ok(CatalogToolEntry {
        tool,
        source: ToolSource::StaticMcp(server),
    })
}

fn dynamic_catalog_tools_of(
    base: &BTreeMap<String, CatalogToolEntry>,
) -> Vec<DynamicMcpCatalogTool> {
    base.iter()
        .map(|(name, entry)| DynamicMcpCatalogTool {
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
        })
        .collect()
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
        .filter(|entry| entry.tool.is_direct() && entry.tool.visible_to_model())
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
