//! MCP Agents：基于标准 Resource 的远端 subagent 配置发现与激活。
//!
//! registry 只从连接池的 `resources/list` 快照投影元数据；正文仅在首次激活时
//! 经 `resources/read` 拉取、校验并按 digest 缓存。远端定义不写入本地 agents
//! 目录，也不会覆盖本地/插件/builtin 定义。

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use rmcp::model::ResourceContents;
use sha2::{Digest, Sha256};

use super::client::McpClientPool;
use crate::claude_agent_parser::{parse_agent_file, ClaudeAgent};

const MAX_AGENT_BYTES: usize = 256 * 1024;
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAgentMetadata {
    pub id: String,
    pub origin: String,
    pub name: String,
    pub description: String,
    pub uri: String,
}

#[derive(Debug, Clone)]
pub struct ActivatedMcpAgent {
    pub metadata: McpAgentMetadata,
    pub definition: ClaudeAgent,
    pub digest: String,
}

pub struct McpAgentRegistry {
    pool: Arc<McpClientPool>,
    activated: RwLock<HashMap<String, ActivatedMcpAgent>>,
    approvals: RwLock<std::collections::HashSet<String>>,
}

impl McpAgentRegistry {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            activated: RwLock::new(HashMap::new()),
            approvals: RwLock::new(std::collections::HashSet::new()),
        }
    }

    pub fn entries(&self) -> Vec<McpAgentMetadata> {
        let mut entries = Vec::new();
        for handle in self.pool.get_all_clients() {
            for resource in &handle.resources {
                let Some(name) = agent_name_from_uri(&resource.uri) else {
                    continue;
                };
                entries.push(McpAgentMetadata {
                    id: mcp_agent_id(&handle.name, &name),
                    origin: handle.name.clone(),
                    name,
                    description: resource.description.clone().unwrap_or_default(),
                    uri: resource.uri.clone(),
                });
            }
        }
        entries.sort_by(|a, b| (&a.origin, &a.name, &a.uri).cmp(&(&b.origin, &b.name, &b.uri)));
        entries
    }

    pub fn resolve(&self, id: &str) -> Result<McpAgentMetadata, String> {
        let matches: Vec<_> = self
            .entries()
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect();
        match matches.as_slice() {
            [] => Err(format!("cannot find MCP agent definition '{id}'")),
            [entry] => Ok(entry.clone()),
            _ => Err(format!(
                "MCP agent definition '{id}' is ambiguous; use an origin-specific ID"
            )),
        }
    }

    pub fn cached(&self, id: &str) -> Option<ActivatedMcpAgent> {
        self.activated.read().get(id).cloned()
    }

    pub async fn activate(&self, id: &str) -> Result<ActivatedMcpAgent, String> {
        let metadata = self.resolve(id)?;
        let handle = self
            .pool
            .get_client(&metadata.origin)
            .filter(|handle| matches!(handle.status, super::client::ClientStatus::Connected))
            .ok_or_else(|| format!("MCP server '{}' is not connected", metadata.origin))?;
        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| format!("MCP server '{}' has no active peer", metadata.origin))?;

        let (result, ticket) = tokio::time::timeout(
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
        if definition.frontmatter.name != metadata.name {
            return Err(format!(
                "MCP agent name '{}' does not match URI name '{}'",
                definition.frontmatter.name, metadata.name
            ));
        }
        if definition.frontmatter.description.trim().is_empty() {
            return Err("MCP agent description must not be empty".to_string());
        }
        normalize_remote_definition(&mut definition)?;

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
            .insert(id.to_string(), activated.clone());
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

pub fn mcp_agent_id(origin: &str, name: &str) -> String {
    format!("mcp__{}__{}", sanitize_id_part(origin), name)
}

fn normalize_remote_definition(definition: &mut ClaudeAgent) -> Result<(), String> {
    if definition.frontmatter.max_turns == Some(0) {
        return Err("MCP agent maxTurns must be a positive integer".to_string());
    }
    if let Some(model) = definition.frontmatter.model.as_deref() {
        let model = model.to_ascii_lowercase();
        if model != "inherit" && !["haiku", "sonnet", "opus", "fable"].contains(&model.as_str()) {
            return Err(format!("unsupported MCP agent model suggestion '{model}'"));
        }
        definition.frontmatter.model = Some(model);
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
    // Skill URI 需要独立校验与批准；当前宿主的 preload seam 只接受本地名称，
    // 因此保守忽略，绝不把激活 Agent 当成对 Skill 的隐式授权。
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
}
