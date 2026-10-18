//! web 实例发布的 builtin Agent 定义资源面。
//!
//! `web-researcher` 的定义随 web 实例发布（`agent://builtin/web-researcher/agent.md`）：
//! 宿主 `McpAgentRegistry` 信任宿主绑定的 builtin 实例来源，因此该定义与 workspace
//! 实例发布的 builtin Agent 在目录面同形（scope = builtin、id 保持裸名），web 实例
//! 关闭时随实例一起不可发现。
//!
//! 边界：
//! - 只提供编译期嵌入的静态资产，不扫描目录、不读宿主配置，不做启用/关闭策略
//!   （§5.1「server 不持策略」）；
//! - frontmatter 解析复用 `peri_mcp_common::agent_definition::frontmatter`，本模块
//!   不解释能力、不因 frontmatter 声称提权；
//! - `_meta` 投影与 workspace 资源面同键：`io.peri/scope` / `io.peri/digest` /
//!   `io.peri/frontmatter`（宿主目录渲染据此推断 model tier / 写能力标签，正文
//!   仅在激活时经 `resources/read` 读取）。

use peri_acp_types::workspace_resources::{
    agent_uri, digest_bytes, is_valid_uri_segment, parse_agent_uri, ResourceScope, META_KEY_DIGEST,
    META_KEY_FRONTMATTER, META_KEY_SCOPE, MIME_MARKDOWN,
};
use peri_mcp_common::agent_definition::frontmatter::frontmatter_json;
use rmcp::model::{MetaObject, Resource, ResourceContents};

/// 单个 builtin Agent 定义的编译期嵌入数据。
pub struct BuiltinAgent {
    /// Agent 标识（`agent://builtin/{id}/agent.md` 的 `{id}` 段）。
    pub id: &'static str,
    /// 定义全文（YAML frontmatter + markdown 正文），编译期嵌入。
    pub content: &'static str,
}

/// web 实例发布的 builtin Agent（顺序即 `resources/list` 的公开顺序）。
pub static BUILTIN_AGENTS: &[BuiltinAgent] = &[BuiltinAgent {
    id: "web-researcher",
    content: include_str!("agents/web-researcher.md"),
}];

/// 读取错误（由 handler 映射为 MCP 错误码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentResourceError {
    /// 非 `agent://` 形态或段非法（`-32602`）。
    InvalidUri,
    /// 形状合法但不在本实例公开表（`-32002`）。
    NotFound,
}

/// 单条可公开定义（URI、digest、frontmatter 都来自同一条目）。
struct AgentRecord {
    agent_id: &'static str,
    display_name: String,
    description: String,
    content: &'static str,
    frontmatter: Option<serde_json::Map<String, serde_json::Value>>,
    digest: String,
}

impl AgentRecord {
    /// 资源 `_meta` 投影（builtin scope + digest + frontmatter JSON）。
    fn meta(&self) -> MetaObject {
        let mut meta = serde_json::Map::new();
        meta.insert(
            META_KEY_SCOPE.to_string(),
            serde_json::Value::String(ResourceScope::Builtin.as_str().to_string()),
        );
        meta.insert(
            META_KEY_DIGEST.to_string(),
            serde_json::Value::String(self.digest.clone()),
        );
        if let Some(frontmatter) = &self.frontmatter {
            meta.insert(
                META_KEY_FRONTMATTER.to_string(),
                serde_json::Value::Object(frontmatter.clone()),
            );
        }
        MetaObject(meta)
    }
}

/// 公开记录批（标识非法的条目跳过并记日志，与 workspace builtin 面同口径）。
fn records() -> Vec<AgentRecord> {
    BUILTIN_AGENTS
        .iter()
        .filter_map(|agent| {
            if !is_valid_uri_segment(agent.id) {
                tracing::warn!(
                    agent = agent.id,
                    "builtin agent 标识不符合 URI 段约束，不公开"
                );
                return None;
            }
            let frontmatter = frontmatter_json(agent.content);
            let (display_name, description) = display_fields(agent.id, frontmatter.as_ref());
            Some(AgentRecord {
                agent_id: agent.id,
                display_name,
                description,
                content: agent.content,
                frontmatter,
                digest: digest_bytes(agent.content.as_bytes()),
            })
        })
        .collect()
}

/// 展示字段：frontmatter `name`（空/缺失回退标识）与 trim 后的 `description`。
fn display_fields(
    agent_id: &str,
    frontmatter: Option<&serde_json::Map<String, serde_json::Value>>,
) -> (String, String) {
    let Some(frontmatter) = frontmatter else {
        return (agent_id.to_string(), String::new());
    };
    let display_name = frontmatter
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(agent_id)
        .to_string();
    let description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    (display_name, description)
}

/// 资源发现批（`resources/list`）。
pub(crate) fn list_resources() -> Vec<Resource> {
    records()
        .into_iter()
        .filter_map(|record| {
            let uri = agent_uri(ResourceScope::Builtin, None, record.agent_id)?;
            let mut resource = Resource::new(uri, record.agent_id.to_string())
                .with_mime_type(MIME_MARKDOWN)
                .with_meta(record.meta());
            if record.display_name != record.agent_id {
                resource = resource.with_title(record.display_name);
            }
            if !record.description.is_empty() {
                resource = resource.with_description(record.description);
            }
            Some(resource)
        })
        .collect()
}

/// 读取定义正文（`resources/read`）。
///
/// 只服务 `agent://builtin/{id}/agent.md` 且 id 在本实例公开表内的 URI；其他
/// agent URI 形状合法但本实例不提供 → [`AgentResourceError::NotFound`]。
pub(crate) fn read(uri: &str) -> Result<ResourceContents, AgentResourceError> {
    let parsed = parse_agent_uri(uri).ok_or(AgentResourceError::InvalidUri)?;
    if parsed.scope != ResourceScope::Builtin || parsed.plugin_name.is_some() {
        return Err(AgentResourceError::NotFound);
    }
    let record = records()
        .into_iter()
        .find(|record| record.agent_id == parsed.name.as_str())
        .ok_or(AgentResourceError::NotFound)?;
    let contents =
        ResourceContents::text(record.content.to_string(), uri).with_mime_type(MIME_MARKDOWN);
    Ok(with_meta(contents, record.meta()))
}

/// 把 `_meta` 投影到 `ResourceContents`（与 workspace `project_resource` 同形）。
fn with_meta(mut contents: ResourceContents, meta: MetaObject) -> ResourceContents {
    match &mut contents {
        ResourceContents::TextResourceContents { meta: slot, .. } => *slot = Some(meta),
        ResourceContents::BlobResourceContents { meta: slot, .. } => *slot = Some(meta),
        // `ResourceContents` 是 non_exhaustive：未来新增变体时保持编译（不投影 meta）。
        _ => {}
    }
    contents
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;
