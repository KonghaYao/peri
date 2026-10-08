//! MCP Agents：基于标准 Resource 的 subagent 配置发现与激活（W5：本地三来源接线）。
//!
//! registry 只从连接池的 `resources/list` 快照投影元数据；正文仅在首次激活时
//! 经 `resources/read` 拉取、校验并按 digest 缓存。远端定义不写入本地 agents
//! 目录，也不会覆盖本地/插件/builtin 定义。
//!
//! ## 来源判定（host-assigned，plan §6.2 / X3 / X6）
//!
//! - **本地来源**（project / plugin / builtin 三类）只认**宿主绑定的** builtin
//!   `workspace` 实例（`ConfigSource::Builtin { instance: "workspace" }` 且
//!   `Connected`，且未被 A24 关闭集关闭）——来源描述取自该实例资源 `_meta` 的
//!   `io.peri/scope` / `io.peri/plugin`，**不信任任何 server 的文本自称**：其他
//!   server（含占用同名 `workspace` 的实例）上的同 scheme 资源一律按远端处理。
//! - **远端来源**：id = `mcp__{server}__{name}`，激活走严格 v1 规范化
//!   （本地扩展字段清空、模型/轮数校验）与内容绑定批准。
//! - **本地 id 保持裸名**（`coder`、`explorer` …）：`subagent_type` 的既有取值
//!   不变；同名跨来源并存，选择优先级（E13 project → builtin → plugin）由
//!   [`McpAgentRegistry::resolve_local`] 表达，**不做跨来源覆盖**。
//!
//! ## 会话 / 关闭过滤（W5 第 1 步）
//!
//! - 只投影**本会话可见**的连接（`get_all_clients_visible_to`，ACP 归属过滤）；
//! - `WorkspaceMiddleware` 关闭（实例进 A24 关闭集）⇒ 本地来源整体不可发现、
//!   不可激活（X4：不回落磁盘）；
//! - `SubAgentMiddleware` 链槽关闭（`local_face_closed`，与关闭集同一份
//!   `disabled_middlewares` 派生）⇒ 同上（Agent 工具面关闭时本地定义不再可用）；
//! - builtin 来源的启用位（`built_in_subagents_enabled`）是**调用方策略**：
//!   目录渲染与新建路径按位跳过 builtin 项，resume 路径允许恢复既有 builtin
//!   定义（与迁移前的 `include_built_ins` 语义逐位一致）。

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::agents::{AgentModelSelection, InvalidModelTier};
use peri_acp_types::workspace_resources::{
    ResourceScope, META_KEY_FRONTMATTER, META_KEY_PLUGIN, META_KEY_SCOPE,
};
use rmcp::model::ResourceContents;
use sha2::{Digest, Sha256};

use super::client::{ClientStatus, McpClientHandle, McpClientPool};
use peri_mcp_core::agent_definition::{parse_agent_file, ClaudeAgent, ClaudeAgentFrontmatter};

const MAX_AGENT_BYTES: usize = 256 * 1024;
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 本地来源的 scope 优先级（E13：project → builtin → plugin）。
///
/// 数字小者优先；未接线的 scope（user/global）排在 plugin 之后，仅作稳定序。
fn local_priority(scope: ResourceScope) -> u8 {
    match scope {
        ResourceScope::Project => 0,
        ResourceScope::Builtin => 1,
        ResourceScope::Plugin => 2,
        ResourceScope::User => 3,
        ResourceScope::Global => 4,
    }
}

/// Agent 来源描述（host-assigned；不信任 server 自报）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSource {
    /// 宿主绑定的 builtin `workspace` 实例提供的本地来源。
    Local {
        scope: ResourceScope,
        plugin_name: Option<String>,
    },
    /// 远端 MCP server（严格 v1）。
    Remote,
}

impl AgentSource {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// 本地来源的 scope（远端为 `None`）。
    pub fn local_scope(&self) -> Option<ResourceScope> {
        match self {
            Self::Local { scope, .. } => Some(*scope),
            Self::Remote => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAgentMetadata {
    /// 消费面标识：本地 = 裸 agent 标识；远端 = `mcp__{server}__{name}`。
    pub id: String,
    /// 展示用来源：本地 = 实例名（`workspace`）；远端 = server 名。
    pub origin: String,
    /// 宿主判定的来源（含 scope/plugin 可追溯身份）。
    pub source: AgentSource,
    pub name: String,
    pub description: String,
    pub uri: String,
    /// 目录展示的模型档位（typed 已验证值；渲染只经 `catalog_label()`）。
    pub model_tier: AgentModelSelection,
    /// 目录展示的写能力标签（保守调度提示，不是授权）。
    pub can_mutate: bool,
}

impl McpAgentMetadata {
    /// 目录候选项（`{{available_agents}}` 渲染用）。
    pub fn catalog_entry(&self) -> peri_acp_types::agents::AgentCatalogEntry {
        peri_acp_types::agents::AgentCatalogEntry {
            id: self.id.clone(),
            model_tier: self.model_tier.clone(),
            can_mutate: self.can_mutate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActivatedMcpAgent {
    pub metadata: McpAgentMetadata,
    pub definition: ClaudeAgent,
    pub digest: String,
}

struct RejectedLocalAgent {
    id: String,
    origin: String,
    source: AgentSource,
    uri: String,
    error: InvalidModelTier,
}

impl RejectedLocalAgent {
    fn priority(&self) -> u8 {
        local_priority(self.source.local_scope().expect("local rejection source"))
    }

    fn blocks(&self, entry: &McpAgentMetadata, include_builtin: bool) -> bool {
        self.id == entry.id
            && (include_builtin || self.source.local_scope() != Some(ResourceScope::Builtin))
            && self.priority()
                <= entry
                    .source
                    .local_scope()
                    .map(local_priority)
                    .unwrap_or(u8::MAX)
    }
}

pub struct McpAgentRegistry {
    pool: Arc<McpClientPool>,
    /// 会话可见性过滤（`None` = 不过滤；ACP 归属过滤的单一入口）。
    session_id: Option<String>,
    /// `SubAgentMiddleware` 链槽关闭位（W5；与关闭集同一份 `disabled_middlewares`）。
    local_face_closed: bool,
    activated: RwLock<HashMap<String, ActivatedMcpAgent>>,
    approvals: RwLock<std::collections::HashSet<String>>,
}

impl McpAgentRegistry {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            session_id: None,
            local_face_closed: false,
            activated: RwLock::new(HashMap::new()),
            approvals: RwLock::new(std::collections::HashSet::new()),
        }
    }

    /// 注入会话标识（ACP 归属过滤；`None` = 不过滤，部署面视图）。
    pub fn with_session(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    /// 注入本地 Agent 面关闭位（`SubAgentMiddleware` 链槽）。
    pub fn with_local_face_closed(mut self, closed: bool) -> Self {
        self.local_face_closed = closed;
        self
    }

    /// **会话级 Agent registry 的唯一构造入口**：会话归属过滤与链槽关闭位一次成形。
    ///
    /// 关闭位与关闭集**同源**——判据是 [`crate::assembly::SUB_AGENT_FACE_CLOSED_KEY`]
    /// 在 `disabled_middlewares` 中的命中与否，与链装配跳过 `SubAgentMiddleware`
    /// 槽位（`crate::assembly::ProductionChainAssembler::assemble`）用的是同一份集合。
    /// 关闭位若在此处派生错（恒 false / 恒 true），就会出现「工具面已关闭、目录面仍
    /// 可见」的裂缝（X4/J5），故不做第二处派生。
    ///
    /// 两个 bind 点都经此构造、共用同一形状：会话创建期的定点绑定（`peri-acp` 的
    /// `host/workspace.rs::assemble_with_frozen`，经由
    /// [`crate::host_ports::bind_agent_catalog_from_pool`]）与 turn 级装配
    /// （`crate::assembly::preparation::resolve_ports`）。
    ///
    /// 不经此入口的构造点**有意不同形**，不要为统一而改：`assembly/workflow.rs` 的
    /// workflow agent 面不做会话过滤（部署面视图），`mcp/middleware.rs` 的
    /// DiscoverMCP 投影取自链上既有实例字段。
    pub fn for_session(
        pool: Arc<McpClientPool>,
        session_id: &str,
        disabled_middlewares: &std::collections::HashSet<String>,
    ) -> Self {
        Self::new(pool)
            .with_session(Some(session_id.to_owned()))
            .with_local_face_closed(
                disabled_middlewares.contains(crate::assembly::SUB_AGENT_FACE_CLOSED_KEY),
            )
    }

    pub fn entries(&self) -> Vec<McpAgentMetadata> {
        self.collect_entries().0
    }

    fn collect_entries(&self) -> (Vec<McpAgentMetadata>, Vec<RejectedLocalAgent>) {
        let mut entries = Vec::new();
        let mut rejected = Vec::new();
        for handle in self
            .pool
            .get_all_clients_visible_to(self.session_id.as_deref())
        {
            // F8（关闭语义）：宿主绑定的 builtin `workspace` 句柄**永远不是远端来源**。
            // 链槽关闭 / A24 关闭集命中 / 断连时该句柄的本地面不可用 ⇒ 整体跳过，
            // 不得把同一批资源投影成 `mcp__workspace__*` 远端条目（X4/J5：关闭 ⇒
            // 不可发现；否则模型仍能经 `definitions.rs` 加载被关闭的本地定义）。
            if Self::is_builtin_workspace_handle(&handle) && !self.is_local_handle(&handle) {
                continue;
            }
            let local_handle = self.is_local_handle(&handle);
            for resource in &handle.resources {
                // 名字合法性**按来源拆分**（F6）：本地走契约段校验（X3：wire 不改写
                // name，历史名如 `code_reviewer` / `MyAgent` 必须继续可用），远端沿用
                // HEAD 的严格集（`[a-z0-9-]`）。URI 解析本身对两者一致。
                let Some(raw_name) = agent_name_from_uri_permissive(&resource.uri) else {
                    continue;
                };
                let name = if local_handle {
                    let Some(name) = local_agent_name(&raw_name) else {
                        continue;
                    };
                    name
                } else {
                    if !is_valid_agent_name(&raw_name) {
                        continue;
                    }
                    raw_name
                };
                if !local_handle {
                    // 远端：严格 v1；目录展示标签取自定义正文（激活期才知道），
                    // 投影面保持保守（inherit/writes）。
                    entries.push(McpAgentMetadata {
                        id: mcp_agent_id(&handle.name, &name),
                        origin: handle.name.clone(),
                        source: AgentSource::Remote,
                        name,
                        description: resource.description.clone().unwrap_or_default(),
                        uri: resource.uri.clone(),
                        model_tier: AgentModelSelection::Inherit,
                        can_mutate: true,
                    });
                    continue;
                }
                // 本地：scope 逐资源取自 `_meta`（provider 产出，宿主绑定的实例），
                // frontmatter 不可解析的条目与旧扫描口径一致地不进候选。
                let Some(source) = Self::local_entry_source(resource) else {
                    tracing::debug!(uri = %resource.uri, "本地 agent 资源缺 scope 元数据，不公开");
                    continue;
                };
                let frontmatter = resource
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.0.get(META_KEY_FRONTMATTER))
                    .and_then(|value| value.as_object());
                let fields = match local_catalog_fields(&name, &resource.description, frontmatter) {
                    Ok(Some(fields)) => fields,
                    Ok(None) => {
                        tracing::debug!(agent = %name, "本地 agent frontmatter 不可解析，不公开");
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(agent = %name, origin = %handle.name, source = ?source,
                            uri = %resource.uri, error = %error, "本地 agent model 档位非法，已隔离");
                        rejected.push(RejectedLocalAgent {
                            id: name,
                            origin: handle.name.clone(),
                            source,
                            uri: resource.uri.clone(),
                            error,
                        });
                        continue;
                    }
                };
                entries.push(McpAgentMetadata {
                    id: name.clone(),
                    origin: handle.name.clone(),
                    source,
                    name: fields.display_name,
                    description: fields.description,
                    uri: resource.uri.clone(),
                    model_tier: fields.model_tier,
                    can_mutate: fields.can_mutate,
                });
            }
        }
        entries.sort_by(|a, b| (&a.origin, &a.name, &a.uri).cmp(&(&b.origin, &b.name, &b.uri)));
        (entries, rejected)
    }

    /// 该句柄是否是**宿主绑定的** builtin `workspace` 实例（身份判定，不含关闭位）。
    fn is_builtin_workspace_handle(handle: &Arc<McpClientHandle>) -> bool {
        super::builtin::is_workspace_source(handle.source.as_ref())
    }

    /// 本地来源判定（host-assigned 信任锚 + 关闭位）。
    fn is_local_handle(&self, handle: &Arc<McpClientHandle>) -> bool {
        if self.local_face_closed {
            return false;
        }
        if !matches!(handle.status, ClientStatus::Connected) {
            return false;
        }
        if !super::builtin::is_workspace_source(handle.source.as_ref()) {
            return false;
        }
        let closed = self
            .pool
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        if super::builtin::is_closed_source("workspace", handle.source.as_ref(), &closed) {
            tracing::debug!(
                "agents: builtin workspace is closed; local agent sources are unavailable"
            );
            return false;
        }
        true
    }

    /// 逐资源的本地 scope（`_meta` 缺失/非法时返回 `None`，该资源不公开）。
    fn local_entry_source(resource: &rmcp::model::Resource) -> Option<AgentSource> {
        let meta = resource.meta.as_ref()?;
        let scope = meta
            .0
            .get(META_KEY_SCOPE)
            .and_then(|value| value.as_str())
            .and_then(ResourceScope::parse)?;
        let plugin_name = meta
            .0
            .get(META_KEY_PLUGIN)
            .and_then(|value| value.as_str())
            .map(str::to_string);
        match (scope, plugin_name) {
            (ResourceScope::Plugin, plugin) => Some(AgentSource::Local {
                scope,
                plugin_name: plugin,
            }),
            // 非 plugin scope 带 plugin 段：URI 不可能合法（provider 不会产出），
            // 保守丢弃而不是猜一个来源；plugin scope 缺 plugin 段由上面的分支
            // 承载（`AgentSource::Local` 的 plugin_name 为 `None`，读取时按
            // scope+plugin 精确匹配即找不到该资源）。
            (_, Some(_)) => None,
            (_, None) => Some(AgentSource::Local {
                scope,
                plugin_name: None,
            }),
        }
    }

    /// 解析任一 id（本地裸名或 `mcp__` 远端 id）。
    ///
    /// 本地 id 走 E13 优先级（`include_builtin=false` 时跳过 builtin 来源）。
    pub fn resolve(&self, id: &str) -> Result<McpAgentMetadata, String> {
        if id.starts_with("mcp__") {
            let matches: Vec<_> = self
                .entries()
                .into_iter()
                .filter(|entry| entry.id == id && !entry.source.is_local())
                .collect();
            return match matches.as_slice() {
                [] => Err(format!("cannot find MCP agent definition '{id}'")),
                [entry] => Ok(entry.clone()),
                _ => Err(format!(
                    "MCP agent definition '{id}' is ambiguous; use an origin-specific ID"
                )),
            };
        }
        self.resolve_local(id, true)
    }

    /// 本地来源按 E13 优先级选择（project → builtin → plugin）。
    ///
    /// 同名跨来源并存：返回的是**最高优先级**条目；其他来源仍可按 URI 读取。
    /// `include_builtin=false`（新建路径在 builtin 关闭时）跳过 builtin 来源。
    pub fn resolve_local(
        &self,
        id: &str,
        include_builtin: bool,
    ) -> Result<McpAgentMetadata, String> {
        let (entries, rejected) = self.collect_entries();
        let mut candidates: Vec<McpAgentMetadata> = entries
            .into_iter()
            .filter(|entry| entry.source.is_local() && entry.id == id)
            .filter(|entry| {
                include_builtin || entry.source.local_scope() != Some(ResourceScope::Builtin)
            })
            .collect();
        candidates.sort_by_key(|entry| match &entry.source {
            AgentSource::Local { scope, .. } => local_priority(*scope),
            AgentSource::Remote => u8::MAX,
        });
        let selected = candidates.into_iter().next();
        if let Some(rejection) = rejected
            .iter()
            .filter(|rejection| {
                rejection.id == id
                    && (include_builtin
                        || rejection.source.local_scope() != Some(ResourceScope::Builtin))
                    && selected
                        .as_ref()
                        .map_or(true, |entry| rejection.blocks(entry, include_builtin))
            })
            .min_by_key(|rejection| (rejection.priority(), &rejection.origin, &rejection.uri))
        {
            return Err(format!(
                "agent '{}' from {:?} ({}, {}) declares {}; fix the definition before use",
                rejection.id, rejection.source, rejection.origin, rejection.uri, rejection.error
            ));
        }
        match selected {
            Some(entry) => Ok(entry),
            None => Err(format!("cannot find agent definition '{id}'")),
        }
    }

    /// 本地候选目录（E13 优先级序，同名仅保留最高优先级项）。
    ///
    /// 目录渲染面用：`include_builtin=false` 时 builtin 项不进候选
    /// （与迁移前 `scan_agents_detailed(..., include_built_ins)` 一致）。
    pub fn local_catalog(&self, include_builtin: bool) -> Vec<McpAgentMetadata> {
        let (entries, rejected) = self.collect_entries();
        let mut entries: Vec<McpAgentMetadata> = entries
            .into_iter()
            .filter(|entry| entry.source.is_local())
            .filter(|entry| {
                include_builtin || entry.source.local_scope() != Some(ResourceScope::Builtin)
            })
            .filter(|entry| {
                !rejected
                    .iter()
                    .any(|rejection| rejection.blocks(entry, include_builtin))
            })
            .collect();
        entries.sort_by(|a, b| {
            let pa = match &a.source {
                AgentSource::Local { scope, .. } => local_priority(*scope),
                AgentSource::Remote => u8::MAX,
            };
            let pb = match &b.source {
                AgentSource::Local { scope, .. } => local_priority(*scope),
                AgentSource::Remote => u8::MAX,
            };
            (pa, &a.id, &a.uri).cmp(&(pb, &b.id, &b.uri))
        });
        let mut seen = std::collections::HashSet::new();
        entries.retain(|entry| seen.insert(entry.id.clone()));
        entries
    }

    pub fn cached(&self, id: &str) -> Option<ActivatedMcpAgent> {
        self.activated.read().get(id).cloned()
    }

    /// 激活定义（本地裸名或 `mcp__` 远端 id；`include_builtin` 仅作用于本地）。
    pub async fn activate(
        &self,
        id: &str,
        include_builtin: bool,
    ) -> Result<ActivatedMcpAgent, String> {
        let metadata = if id.starts_with("mcp__") {
            self.resolve(id)?
        } else {
            self.resolve_local(id, include_builtin)?
        };
        self.activate_metadata(metadata).await
    }

    async fn activate_metadata(
        &self,
        metadata: McpAgentMetadata,
    ) -> Result<ActivatedMcpAgent, String> {
        let handle = self
            .pool
            .get_client(&metadata.origin)
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .ok_or_else(|| format!("MCP server '{}' is not connected", metadata.origin))?;
        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| format!("MCP server '{}' has no active peer", metadata.origin))?;

        let (result, ticket) = peri_time::timeout(
            READ_TIMEOUT,
            self.pool
                .read_resource_cached(&metadata.origin, &metadata.uri, peer),
        )
        .await
        .map_err(|_| format!("reading MCP agent '{}' timed out", metadata.id))?
        .map_err(|error| format!("failed to read MCP agent '{}': {error}", metadata.id))?;

        if result.contents.len() != 1 {
            return Err(
                "MCP agent resource must contain exactly one text content item".to_string(),
            );
        }
        let text = match &result.contents[0] {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                if mime_type
                    .as_deref()
                    .is_some_and(|mime| mime != "text/markdown" && mime != "text/plain")
                {
                    return Err("MCP agent resource must use a Markdown text mimeType".to_string());
                }
                text
            }
            _ => return Err("MCP agent resource must be UTF-8 text".to_string()),
        };
        if text.len() > MAX_AGENT_BYTES {
            return Err(format!(
                "MCP agent resource exceeds the {} byte limit",
                MAX_AGENT_BYTES
            ));
        }

        let mut definition = parse_agent_file(text)
            .ok_or_else(|| "failed to parse MCP agent YAML frontmatter".to_string())?;
        if definition.frontmatter.description.trim().is_empty() {
            return Err("MCP agent description must not be empty".to_string());
        }
        if metadata.source.is_local() {
            // X3-A：本地受信来源保留既定 host 扩展 profile（permission_mode /
            // hooks / memory / skills 等本地语义不被远端规范化清空）。
            if definition.frontmatter.name.trim().is_empty() {
                definition.frontmatter.name = metadata.id.clone();
            }
            // M2：本地/插件 frontmatter `model` 与远端共用 typed 校验与归一。
            // 激活读到的正文与目录投影的 `_meta.frontmatter` 是两个来源一致性
            // 缝隙，故以正文为**启动权威**再校验：非法档位在点名启动处明确失败
            // ——不静默 inherit，也不阻断会话内其他定义；错误只报错误类别，
            // 不回显可疑原始值。
            let selection = AgentModelSelection::parse(definition.frontmatter.model.as_deref())
                .map_err(|InvalidModelTier| {
                    format!(
                        "agent '{}' declares an unsupported model tier; fix the definition before use",
                        metadata.id
                    )
                })?;
            definition.frontmatter.model = selection.normalized_value().map(str::to_string);
        } else {
            if definition.frontmatter.name != metadata_id_name(&metadata) {
                return Err(format!(
                    "MCP agent name '{}' does not match URI name '{}'",
                    definition.frontmatter.name,
                    metadata_id_name(&metadata)
                ));
            }
            normalize_remote_definition(&mut definition)?;
        }

        let digest = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        let activated = ActivatedMcpAgent {
            metadata,
            definition,
            digest,
        };
        self.pool
            .cache_verified_resource(&activated.metadata.origin, ticket, &result)
            .await;
        self.activated
            .write()
            .insert(activated.metadata.id.clone(), activated.clone());
        Ok(activated)
    }

    pub fn approval_key(agent: &ActivatedMcpAgent, effective_tools: &[String]) -> String {
        let mut tools = effective_tools.to_vec();
        tools.sort();
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            agent.metadata.origin,
            agent.metadata.uri,
            agent.digest,
            tools.join("\0"),
            agent
                .definition
                .frontmatter
                .model
                .as_deref()
                .unwrap_or("inherit"),
            agent.definition.frontmatter.max_turns.unwrap_or(200),
        )
    }

    pub fn is_approved(&self, key: &str) -> bool {
        self.approvals.read().contains(key)
    }

    pub fn approve(&self, key: String) {
        self.approvals.write().insert(key);
    }
}

/// 测试夹具（`#[doc(hidden)]`，非生产 API）：用**合成句柄**构造 registry。
///
/// 句柄形状与生产一致（`ConfigSource::Builtin { instance: "workspace" }` +
/// Connected + provider 的 `_meta` 投影），但**没有 peer**——只覆盖目录/来源/
/// 开关判定；正文激活（`resources/read`）由 provider 用例与端到端用例覆盖。
/// 每个条目是 `(scope, agent_id, frontmatter_json)`。
impl McpAgentRegistry {
    #[doc(hidden)]
    pub fn from_local_catalog_for_test(
        entries: &[(ResourceScope, String, String)],
    ) -> McpAgentRegistry {
        use rmcp::model::{MetaObject, Resource};
        let mut resources = Vec::new();
        for (scope, id, frontmatter) in entries {
            let Some(uri) = peri_acp_types::workspace_resources::agent_uri(*scope, None, id) else {
                continue;
            };
            let mut meta = serde_json::Map::new();
            meta.insert(
                META_KEY_SCOPE.to_string(),
                serde_json::Value::String(scope.as_str().to_string()),
            );
            if let Ok(frontmatter) = serde_json::from_str::<serde_json::Value>(frontmatter) {
                meta.insert(META_KEY_FRONTMATTER.to_string(), frontmatter);
            }
            resources.push(
                Resource::new(uri, id.clone())
                    .with_mime_type("text/markdown")
                    .with_meta(MetaObject(meta)),
            );
        }
        let pool = Arc::new(McpClientPool::new_pending());
        pool.clients.write().insert(
            "workspace".to_string(),
            Arc::new(McpClientHandle {
                name: "workspace".to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: Vec::new(),
                resources,
                status: ClientStatus::Connected,
                oauth_status: Default::default(),
                source: Some(super::config::ConfigSource::Builtin {
                    instance: "workspace".to_string(),
                }),
                url: None,
                skills_capable: false,
            }),
        );
        McpAgentRegistry::new(pool)
    }
}

/// 远端 id 对应的 URI 名段（`mcp__{server}__{name}` → `{name}` 由 metadata 的
/// `uri` 决定，不反解 id——id 里的分隔符 `__` 不是可靠边界）。
fn metadata_id_name(metadata: &McpAgentMetadata) -> String {
    agent_name_from_uri(&metadata.uri).unwrap_or_else(|| metadata.id.clone())
}

/// 本地条目的目录展示字段（frontmatter 不可解析时返回 `None`——该条目不进
/// 候选，与旧 `scan_agents_detailed` 的 `parse_agent_file` 成功口径一致）。
struct LocalCatalogFields {
    display_name: String,
    description: String,
    model_tier: AgentModelSelection,
    can_mutate: bool,
}

fn local_catalog_fields(
    agent_id: &str,
    resource_description: &Option<String>,
    frontmatter: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<Option<LocalCatalogFields>, InvalidModelTier> {
    let Some(frontmatter) = frontmatter else {
        return Ok(None);
    };
    let Ok(frontmatter) = serde_json::from_value::<ClaudeAgentFrontmatter>(
        serde_json::Value::Object(frontmatter.clone()),
    ) else {
        return Ok(None);
    };
    let capability = crate::subagent::infer_agent_capability(&frontmatter)?;
    let display_name = if frontmatter.name.trim().is_empty() {
        agent_id.to_string()
    } else {
        frontmatter.name.clone()
    };
    let description = if frontmatter.description.trim().is_empty() {
        resource_description.clone().unwrap_or_default()
    } else {
        frontmatter.description.trim().to_string()
    };
    Ok(Some(LocalCatalogFields {
        display_name,
        description,
        model_tier: capability.model_tier,
        can_mutate: capability.can_mutate,
    }))
}

pub fn mcp_agent_id(origin: &str, name: &str) -> String {
    format!("mcp__{}__{}", sanitize_id_part(origin), name)
}

fn normalize_remote_definition(definition: &mut ClaudeAgent) -> Result<(), String> {
    if definition.frontmatter.max_turns == Some(0) {
        return Err("MCP agent maxTurns must be a positive integer".to_string());
    }
    if let Some(raw) = definition.frontmatter.model.clone() {
        // 复用契约层 typed 解析（与本地/插件来源同一名单与归一规则）；
        // 错误文本不回显可疑原始值。
        let selection = AgentModelSelection::parse(Some(&raw))
            .map_err(|InvalidModelTier| "unsupported MCP agent model suggestion".to_string())?;
        definition.frontmatter.model = selection.normalized_value().map(str::to_string);
    }

    // MCPP v1：这些本地扩展字段具有执行/持久化语义，远端配置默认忽略。
    definition.frontmatter.permission_mode = None;
    definition.frontmatter.mcp_servers.clear();
    definition.frontmatter.hooks = serde_yaml::Value::Null;
    definition.frontmatter.memory = None;
    definition.frontmatter.background = false;
    definition.frontmatter.isolation = None;
    definition.frontmatter.allowed_write_dirs.clear();
    definition.frontmatter.tone = None;
    definition.frontmatter.proactiveness = None;
    definition.frontmatter.prompt_mode = None;
    // 远端 `skills` 声明**不是**隐式授权：技能级批准面当前不存在（W2b 核实，
    // 见 `mcp/skill_activation.rs` 的批准面结论），preload 注入不经过任何批准门
    // ⇒ 保守清空（与迁移前 HEAD 的 rationale 一致）。本地受信来源**不**走本函数
    // （X3-A），其 `skills` 由统一 activation 在预载面逐项校验，不受此处影响。
    definition.frontmatter.skills.clear();
    Ok(())
}

fn sanitize_id_part(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// URI 名字段提取（**不做字符集判定**：合法性按来源拆分，见 `entries()`）。
fn agent_name_from_uri_permissive(uri: &str) -> Option<String> {
    let parsed = url::Url::parse(uri).ok()?;
    if parsed.scheme() != "agent" || parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    let mut parts: Vec<&str> = parsed
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    if parts.pop()? != "agent.md" {
        return None;
    }
    parts
        .pop()
        .or_else(|| parsed.host_str())
        .map(str::to_string)
}

/// 远端严格集（HEAD 口径）：`[a-z0-9-]`、首尾字母数字、长度 ≤128。
fn agent_name_from_uri(uri: &str) -> Option<String> {
    let parsed = url::Url::parse(uri).ok()?;
    if parsed.scheme() != "agent" || parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    let mut parts: Vec<&str> = parsed
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    if parts.pop()? != "agent.md" {
        return None;
    }
    let name = parts.pop().or_else(|| parsed.host_str())?;
    is_valid_agent_name(name).then(|| name.to_string())
}

/// 本地 agent 名的合法化（F6）：契约段校验 + 长度上限 + 非 `mcp__` 前缀。
///
/// 与远端严格集（[`is_valid_agent_name`]，沿用 HEAD 口径）分开：本地历史名字
/// （`code_reviewer` / `MyAgent`）迁移前可用，收窄成严格集会造成静默不可用。
/// 被拒时记 **warn**（不是 debug），便于用户发现。
fn local_agent_name(name: &str) -> Option<String> {
    if name.len() > 128 {
        tracing::warn!(agent = name, "本地 agent 名超过长度上限，不公开");
        return None;
    }
    if name.starts_with("mcp__") {
        tracing::warn!(
            agent = name,
            "本地 agent 名占用 `mcp__` 前缀（远端命名空间），不公开"
        );
        return None;
    }
    if !peri_acp_types::workspace_resources::is_valid_uri_segment(name) {
        tracing::warn!(agent = name, "本地 agent 名不符合资源段约束，不公开");
        return None;
    }
    Some(name.to_string())
}

fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

#[cfg(test)]
#[path = "agent_registry_resource_test.rs"]
mod resource_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_agent_uris() {
        assert_eq!(
            agent_name_from_uri("agent://code-reviewer/agent.md").as_deref(),
            Some("code-reviewer")
        );
        assert_eq!(
            agent_name_from_uri("agent://acme/review/code-reviewer/agent.md").as_deref(),
            Some("code-reviewer")
        );
    }

    #[test]
    fn strips_host_local_fields_from_remote_definition() {
        let mut definition = parse_agent_file(
            "---\nname: reviewer\ndescription: review\npermissionMode: bypassPermissions\nmcpServers:\n  - arbitrary\nhooks:\n  PreToolUse: []\nmemory: project\nbackground: true\nisolation: worktree\nallowedWriteDirs: [tmp]\nskills: [skill://review/SKILL.md]\n---\nReview code.",
        )
        .unwrap();

        normalize_remote_definition(&mut definition).unwrap();

        assert!(definition.frontmatter.permission_mode.is_none());
        assert!(definition.frontmatter.mcp_servers.is_empty());
        assert_eq!(definition.frontmatter.hooks, serde_yaml::Value::Null);
        assert!(definition.frontmatter.memory.is_none());
        assert!(!definition.frontmatter.background);
        assert!(definition.frontmatter.isolation.is_none());
        assert!(definition.frontmatter.allowed_write_dirs.is_empty());
        // F3（安全）：远端 `skills` 清空——技能级批准面不存在，声明不构成隐式授权。
        assert!(definition.frontmatter.skills.is_empty());
    }

    #[test]
    fn rejects_unsupported_remote_model() {
        let mut definition = parse_agent_file(
            "---\nname: reviewer\ndescription: review\nmodel: unrestricted-model\n---\nReview code.",
        )
        .unwrap();

        assert!(normalize_remote_definition(&mut definition).is_err());
    }

    #[test]
    fn rejects_invalid_agent_uris() {
        assert!(agent_name_from_uri("agent://Reviewer/agent.md").is_none());
        assert!(agent_name_from_uri("agent://reviewer/AGENT.md").is_none());
        assert!(agent_name_from_uri("skill://reviewer/agent.md").is_none());
    }

    #[test]
    fn local_trusted_definition_keeps_host_extensions_while_remote_is_strict_v1() {
        // X3-A：本地受信 origin 保留既定 host 扩展 profile；远端严格 v1。
        // （agent 批准不覆盖 skill：两侧的 `skills` 条目都留在 frontmatter，
        // 由统一 Skill activation 逐项校验——远端不再被整体清空。）
        let text = "---\nname: reviewer\ndescription: review\npermissionMode: acceptEdits\nmemory: project\nskills: [\"skill://review/SKILL.md\"]\n---\nReview code.";
        let mut remote = parse_agent_file(text).unwrap();
        normalize_remote_definition(&mut remote).unwrap();
        assert!(
            remote.frontmatter.permission_mode.is_none(),
            "远端清空本地扩展"
        );
        assert!(
            remote.frontmatter.skills.is_empty(),
            "远端 skills 清空（技能级批准面不存在 ⇒ 不构成隐式授权）"
        );

        let local = parse_agent_file(text).unwrap();
        assert_eq!(
            local.frontmatter.permission_mode.as_deref(),
            Some("acceptEdits"),
            "本地受信来源保留 host 扩展 profile"
        );
        assert_eq!(local.frontmatter.memory.as_deref(), Some("project"));
        assert_eq!(local.frontmatter.skills.len(), 1);
    }

    #[test]
    fn local_priority_follows_e13() {
        assert!(local_priority(ResourceScope::Project) < local_priority(ResourceScope::Builtin));
        assert!(local_priority(ResourceScope::Builtin) < local_priority(ResourceScope::Plugin));
    }
}
