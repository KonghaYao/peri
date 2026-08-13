//! DiscoverMCP —— 只读 MCP 域查询工具（deferred、namespace=meta）。
//!
//! JSON-RPC 风格入参（`{method, params}`），错误契约 `{error:{code,message}}`
//! （无 id）：未知 method `-32601`、参数错误 `-32602`、server 不存在/未连接
//! `-32000`。**只读硬约束**：本文件不调用任何 MCP tools/call、read_resource、
//! ExecuteExtraTool、build_tool_bridges——数据全部来自 pool 只读快照与
//! registry。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    mcp_skills::McpSkillRegistry,
    skills::{SkillMetadata, SkillOrigin},
};
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::model::Tool;
use serde_json::{json, Value};

use super::{
    client::{ClientStatus, McpClientPool, OAuthStatus},
    config::ConfigSource,
};

const TOOL_NAME: &str = "DiscoverMCP";
const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_CAP: usize = 20;

/// 只读查询 MCP 域：搜索 MCP server/tool/resource/skill、列出服务器清单、
/// 查看服务器连接详情。不提供执行（工具走 ExecuteExtraTool、资源走
/// mcp_read_resource、技能走 SkillTool）。
pub struct DiscoverMCPTool {
    pool: Arc<McpClientPool>,
    registry: Option<Arc<McpSkillRegistry>>,
}

impl DiscoverMCPTool {
    pub fn new(pool: Arc<McpClientPool>, registry: Option<Arc<McpSkillRegistry>>) -> Self {
        Self { pool, registry }
    }

    fn search(&self, params: &Value) -> Value {
        let Some(query) = params.get("query") else {
            return err_obj(-32602, "缺少 query 参数（string）");
        };
        let Some(query) = query.as_str() else {
            return err_obj(-32602, "query must be a string");
        };
        let max_results = match params.get("max_results") {
            None | Some(Value::Null) => DEFAULT_MAX_RESULTS,
            // 负数 / 浮点（as_u64 返回 None）不得静默归零——按参数错误契约返回。
            Some(Value::Number(n)) => match n.as_u64() {
                Some(v) => v as usize,
                None => return err_obj(-32602, "max_results must be a non-negative integer"),
            },
            Some(_) => return err_obj(-32602, "max_results 必须是整数"),
        }
        .min(MAX_RESULTS_CAP);
        let needle = query.to_lowercase();

        let mut results: Vec<Value> = Vec::new();

        // server：全部已配置/已连接服务器（名称匹配）
        for info in self.pool.all_server_infos() {
            if info.name.to_lowercase().contains(&needle) {
                results.push(json!({
                    "type": "server",
                    "name": info.name,
                    "transport_type": info.transport_type,
                    "status": status_str(&info.status),
                    "tool_count": info.tool_count,
                    "resource_count": info.resource_count,
                    "oauth_status": oauth_status_str(&info.oauth_status),
                    "url": info.url,
                }));
            }
        }

        // tool / resource：已连接 server 的缓存快照（get_all_clients 只返回 Connected）
        for handle in self.pool.get_all_clients() {
            for tool in &handle.tools {
                let name_hit = tool.name.to_lowercase().contains(&needle);
                let desc_hit = tool
                    .description
                    .as_ref()
                    .map(|d| d.to_lowercase().contains(&needle))
                    .unwrap_or(false);
                if name_hit || desc_hit {
                    let tool_value =
                        serde_json::to_value(tool).unwrap_or_else(|_| manual_tool_json(tool));
                    results.push(json!({
                        "type": "tool",
                        "server": handle.name,
                        "tool": tool_value,
                    }));
                }
            }
            for resource in &handle.resources {
                if resource.uri.to_lowercase().contains(&needle) {
                    results.push(json!({
                        "type": "resource",
                        "server": handle.name,
                        "uri": resource.uri,
                    }));
                }
            }
        }

        // skill：远端注册表（名称/描述匹配）
        if let Some(registry) = &self.registry {
            for skill in registry.all_skills() {
                let name_hit = skill.name.to_lowercase().contains(&needle);
                let desc_hit = skill.description.to_lowercase().contains(&needle);
                if name_hit || desc_hit {
                    results.push(json!({
                        "type": "skill",
                        "server": skill_server_of(&skill),
                        "name": skill.name,
                        "description": skill.description,
                    }));
                }
            }
        }

        // 确定性（ARC-SERIAL-001）：结果来源含 HashMap / 快照迭代，顺序不定——
        // 统一构造排序键消除同 type 并列的跨运行抖动后再截断：
        // tool 条目 (type, server, tool.name)、resource 条目 (type, server, uri)、
        // server/skill 条目 (type, name)。
        results.sort_by_key(sort_key);
        results.truncate(max_results);
        Value::Array(results)
    }

    fn list(&self, params: &Value) -> Value {
        let Some(server) = params.get("server").and_then(Value::as_str) else {
            return err_obj(-32602, "缺少 server 参数（string）");
        };
        let Some(handle) = self.pool.get_client(server) else {
            return err_obj(-32000, format!("MCP 服务器 \"{server}\" 不存在"));
        };
        if !matches!(&handle.status, ClientStatus::Connected) {
            return err_obj(
                -32000,
                format!("MCP 服务器 \"{server}\" 未连接 (状态: {:?})", handle.status),
            );
        }
        let domain = match params.get("domain") {
            None | Some(Value::Null) => None,
            Some(Value::String(d)) => Some(d.as_str()),
            Some(_) => return err_obj(-32602, "domain 必须是字符串"),
        };

        let tools: Vec<String> = handle.tools.iter().map(|t| t.name.to_string()).collect();
        let resources: Vec<String> = handle.resources.iter().map(|r| r.uri.clone()).collect();
        let skills: Vec<String> = self.skill_names_of(server);

        match domain {
            None => json!({
                "server": server,
                "tools": tools,
                "resources": resources,
                "skills": skills,
            }),
            Some("tools") => Value::Array(tools.into_iter().map(Value::String).collect()),
            Some("resources") => Value::Array(resources.into_iter().map(Value::String).collect()),
            Some("skills") => Value::Array(skills.into_iter().map(Value::String).collect()),
            Some(other) => err_obj(
                -32602,
                format!("未知 domain: {other}（tools/resources/skills）"),
            ),
        }
    }

    fn detail(&self, params: &Value) -> Value {
        let Some(server) = params.get("server").and_then(Value::as_str) else {
            return err_obj(-32602, "缺少 server 参数（string）");
        };
        let Some(handle) = self.pool.get_client(server) else {
            return err_obj(-32000, format!("MCP 服务器 \"{server}\" 不存在"));
        };
        // detail 只服务已连接 server（spec 错误表：已配置但未连接 → -32000）
        if !matches!(&handle.status, ClientStatus::Connected) {
            return err_obj(
                -32000,
                format!("MCP 服务器 \"{server}\" 未连接 (状态: {:?})", handle.status),
            );
        }

        let mut obj = json!({
            "server": server,
            "status": status_str(&handle.status),
            "oauth_status": oauth_status_str(&handle.oauth_status),
            "source": handle.source.as_ref().map(config_source_str),
            "url": handle.url,
            "tool_count": handle.tools.len(),
            "resource_count": handle.resources.len(),
            "skill_count": self.skill_names_of(server).len(),
        });

        if let Some(peer) = &handle.peer {
            if let Some(info) = peer.peer_info() {
                obj["protocol_version"] =
                    serde_json::to_value(&info.protocol_version).unwrap_or(Value::Null);
                obj["capabilities"] =
                    serde_json::to_value(&info.capabilities).unwrap_or(Value::Null);
            }
        }

        obj
    }

    fn skill_names_of(&self, server: &str) -> Vec<String> {
        self.registry
            .as_ref()
            .map(|reg| reg.skills_of(server).into_iter().map(|s| s.name).collect())
            .unwrap_or_default()
    }
}

/// JSON-RPC 风格错误对象（无 id）。
fn err_obj(code: i64, message: impl Into<String>) -> Value {
    json!({ "error": { "code": code, "message": message.into() } })
}

/// 统一排序键：tool 条目用 (type, server, tool.name)、resource 条目用
/// (type, server, uri)、server/skill 条目用 (type, name)。用 JSON 值字段拼
/// 键，消除同 type 多条目时 HashMap 迭代顺序带来的跨运行抖动
/// （ARC-SERIAL-001），保证截断子集确定。
fn sort_key(v: &Value) -> (String, String, String) {
    let ty = v
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match ty.as_str() {
        "tool" => (
            ty,
            v.get("server")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            v.get("tool")
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        "resource" => (
            ty,
            v.get("server")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            v.get("uri")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        _ => (
            ty,
            String::new(),
            v.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
    }
}

fn status_str(status: &ClientStatus) -> &'static str {
    match status {
        ClientStatus::Connected => "connected",
        ClientStatus::Failed(_) => "failed",
        ClientStatus::Disconnected => "disconnected",
        ClientStatus::Disabled => "disabled",
        ClientStatus::Uninitialized => "uninitialized",
    }
}

fn oauth_status_str(status: &OAuthStatus) -> &'static str {
    match status {
        OAuthStatus::None => "none",
        OAuthStatus::Authorized => "authorized",
        OAuthStatus::NeedsAuthorization => "needs_authorization",
    }
}

fn config_source_str(source: &ConfigSource) -> &'static str {
    match source {
        ConfigSource::Project(_) => "project",
        ConfigSource::Global(_) => "global",
        ConfigSource::Plugin => "plugin",
    }
}

/// rmcp Tool 序列化失败的兜底（按字段手工构造，input_schema 完整带上）。
fn manual_tool_json(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description.as_deref(),
        "input_schema": tool.input_schema.as_ref().clone(),
    })
}

/// skill 所属 server：优先 origin，退化从 `mcp__<server>__<skill>` 全名解析。
fn skill_server_of(skill: &SkillMetadata) -> String {
    if let Some(SkillOrigin::Mcp { server, .. }) = &skill.origin {
        return server.clone();
    }
    skill
        .name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__").map(|(s, _)| s.to_string()))
        .unwrap_or_default()
}

#[async_trait]
impl BaseTool for DiscoverMCPTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "只读查询 MCP 域（JSON-RPC 风格，method ∈ search/list/detail）：\
         search 全域搜索 MCP server/tool/resource/skill（结果带 type 标注，\
         tool 带完整 JSON Schema）；list 列出指定服务器三域清单；detail 查看\
         服务器连接状态/协议版本/capabilities/OAuth。不提供任何执行——MCP \
         工具调用走 ExecuteExtraTool，资源读取走 mcp_read_resource，技能加载走 SkillTool。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "method": { "type": "string", "enum": ["search", "list", "detail"] },
                "params": { "type": "object" }
            },
            "required": ["method"]
        })
    }

    fn namespace(&self) -> Option<&str> {
        Some("meta")
    }

    // is_direct 不覆写（默认 false = deferred）；timeout 用默认。

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 错误也走 Ok 返回错误对象（不 Err）——JSON-RPC 错误契约。
        let Some(method) = input.get("method").and_then(Value::as_str) else {
            return Ok(err_obj(-32602, "缺少 method 参数（search/list/detail）").to_string());
        };
        let params = match input.get("params") {
            None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
            Some(obj @ Value::Object(_)) => obj.clone(),
            Some(_) => return Ok(err_obj(-32602, "params 必须是对象").to_string()),
        };

        let result = match method {
            "search" => self.search(&params),
            "list" => self.list(&params),
            "detail" => self.detail(&params),
            other => err_obj(-32601, format!("未知 method: {other}")),
        };
        Ok(result.to_string())
    }
}

#[cfg(test)]
#[path = "discover_tool_test.rs"]
mod tests;
