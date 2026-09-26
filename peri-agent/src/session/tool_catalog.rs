use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::{
    builtin_mcp::original_tool_name_of_effective,
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
    /// `--disallowed-tools` / agent `tools:` 的 allow/deny 过滤（大小写不敏感精确匹配）。
    ///
    /// **A4 ⑦（IF-D6 匹配型归一）**：builtin 一等工具的模型面名字是 effective name
    /// （如 `mcp__web__WebSearch`），用户配置里既可能写迁移前的裸名，也可能写迁移后的
    /// effective name。因此两侧都按「**原样优先，未命中再用归一后的原始名**」展开候选：
    /// 任一侧候选相等即命中——`--disallowed-tools WebSearch` 与
    /// `--disallowed-tools mcp__web__WebSearch` 都继续生效，且不会新增字面量分支
    /// （归一表只有 `peri_acp_types::builtin_mcp` 一份，IF-D15）。
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
                    .flat_map(|name| name_candidates(&name))
                    .collect(),
            ),
        };
        let disallowed: Vec<String> = disallowed
            .into_iter()
            .flat_map(|name| name_candidates(&name))
            .collect();
        Arc::new(move |name| {
            let candidates = name_candidates(name);
            let matches =
                |configured: &str| candidates.iter().any(|candidate| candidate == configured);
            let allowed = match &policy {
                ToolFilterPolicy::InheritAll => true,
                ToolFilterPolicy::AllowNone => false,
                ToolFilterPolicy::AllowList(names) => names
                    .iter()
                    .any(|candidate| candidate == "*" || matches(candidate)),
            };
            allowed && !disallowed.iter().any(|candidate| matches(candidate))
        })
    }
}

/// 按名过滤的候选集合（A4 ⑦ 匹配型归一）：原样（小写化）恒在其中，命中归一表时
/// 再补一个原始工具名候选。
///
/// 归一表是 [`original_tool_name_of_effective`]（IF-D15 唯一入口）；未命中
/// （未知 / 外部 `mcp__*`）时只有一个候选 ⇒ 与迁移前的单名比较逐位一致。
/// 名字字面量只在声明表里声明一份，本模块不得复制或反拆。
fn name_candidates(name: &str) -> Vec<String> {
    let lowered = name.to_lowercase();
    match original_tool_name_of_effective(name) {
        Some(original) => vec![lowered, original.to_lowercase()],
        None => vec![lowered],
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

/// A4 ⑦（`ToolFilterPolicy::canonical` 的匹配型归一）专属断言。
///
/// 挂载在实现模块内而非 `tool_catalog_test.rs`：`canonical` 的归一改造把断言与
/// 实现保持同一文件（`--disallowed-tools` / agent `tools:` 是**用户面**行为，
/// 裸名与 effective name 两种写法都必须继续生效）。
#[cfg(test)]
mod filter_policy_parity_tests {
    use super::*;

    /// 声明表里的 effective name（字面量只在 `peri_acp_types::builtin_mcp` 声明一份）。
    fn effective_name(instance: &str, original_name: &str) -> &'static str {
        peri_acp_types::builtin_mcp::find(instance)
            .and_then(|declared| {
                declared
                    .tools
                    .iter()
                    .find(|tool| tool.original_name == original_name)
            })
            .map(|tool| tool.effective_name)
            .expect("builtin 声明表应声明该 (实例, 原始工具名)")
    }

    /// 用户写裸名或写 effective name 都必须继续被 deny（迁移不改变用户面语义）。
    #[test]
    fn canonical_disallowed_accepts_naked_and_effective_names() {
        let effective = effective_name("web", "WebFetch");
        for entry in ["WebFetch", "webfetch", effective] {
            let filter = ToolFilterPolicy::canonical(None, vec![entry.to_string()]);
            assert!(!filter(effective), "deny 条目 {entry} 必须命中 {effective}");
            assert!(!filter("WebFetch"), "deny 条目 {entry} 必须命中裸名");
            assert!(filter("Read"), "无关工具不受 deny 影响");
        }
        // 大小写边界：全小写的 effective name 条目按大小写不敏感面仍命中 effective name
        // 本身，但 IF-D15 的归一表是**精确字符串相等**（不折叠大小写），因此不反推裸名
        // 候选。用户面口径是「写 effective name 或写裸名」，两种写法都在上面的循环里。
        let lowered = effective.to_lowercase();
        let filter = ToolFilterPolicy::canonical(None, vec![lowered]);
        assert!(!filter(effective), "全小写条目仍按大小写不敏感命中");
        assert!(
            filter("WebFetch"),
            "全小写 effective 条目不反推裸名（归一表精确匹配，非用户面口径）"
        );
    }

    /// allow 列表同理：两种写法都放行声明表里的 builtin 工具。
    #[test]
    fn canonical_allow_list_accepts_naked_and_effective_names() {
        let search = effective_name("web", "WebSearch");
        let fetch = effective_name("web", "WebFetch");
        for entry in ["WebSearch", search] {
            let filter = ToolFilterPolicy::canonical(Some(vec![entry.to_string()]), vec![]);
            assert!(filter(search), "allow 条目 {entry} 必须放行 {search}");
            assert!(filter("WebSearch"), "allow 条目 {entry} 必须放行裸名");
            assert!(!filter(fetch), "未列入 allow 的工具照旧被过滤");
        }
        // 空 allow = 全禁（既有语义不变）
        let none = ToolFilterPolicy::canonical(Some(vec![]), vec![]);
        assert!(!none(search));
    }

    /// 反证：未知 / 外部 `mcp__*` 的过滤行为与迁移前逐位一致（归一未命中 ⇒ 单名比较）。
    #[test]
    fn canonical_unknown_names_behave_exactly_as_before() {
        let filter = ToolFilterPolicy::canonical(None, vec!["mcp__filesystem__read_file".into()]);
        assert!(
            !filter("mcp__filesystem__read_file"),
            "原样精确匹配照旧生效"
        );
        // 大小写不匹配的 effective name（非冻结字面量）不命中归一表
        let lowered = effective_name("web", "WebSearch").to_lowercase();
        assert!(
            filter(lowered.as_str()),
            "未命中归一表的名字不受 builtin 归一影响"
        );
        assert!(
            original_tool_name_of_effective(&lowered).is_none(),
            "未命中归一表 ⇒ 候选只有原样"
        );
        assert!(
            original_tool_name_of_effective(effective_name("web", "WebSearch")).is_some(),
            "对照组：冻结字面量必须命中归一表"
        );
        // 通配符语义不变：`*` 只对 allow 生效
        let wildcard = ToolFilterPolicy::canonical(Some(vec!["*".into()]), vec![]);
        assert!(wildcard("anything"));
        let deny_wildcard = ToolFilterPolicy::canonical(None, vec!["*".into()]);
        assert!(
            deny_wildcard("anything"),
            "deny 侧的 * 不是通配（既有语义）"
        );
    }
}
