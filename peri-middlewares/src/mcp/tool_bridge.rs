use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use rmcp::model::{ContentBlock, Tool};
use thiserror::Error;

use super::client::{McpClientHandle, McpClientPool};
use crate::tools::output_persist::persist_truncated_output;

/// MCP 工具调用错误
#[derive(Debug, Error)]
pub enum ToolCallError {
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected { server: String, status: String },
    #[error("MCP server \"{server}\" is draining or closed")]
    Unavailable { server: String },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用失败: {reason}")]
    CallFailed {
        server: String,
        tool: String,
        reason: String,
    },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用超时 ({timeout_secs}s)")]
    Timeout {
        server: String,
        tool: String,
        timeout_secs: u64,
    },
}

/// 将单个 MCP tool 包装为 BaseTool 实现
///
/// `Clone` 只复制已有的 String/Value/Arc/gate 字段，不建立新连接、不注册新 lease；
/// 供准入路径从已验证快照多次产出 Box，避免重复注册同一工具。
#[derive(Clone)]
pub struct McpToolBridge {
    server_name: String,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: serde_json::Value,
    model_visible: bool,
    /// 是否直接进入模型 tools 参数；缺省 false（deferred）。
    direct: bool,
    server_generation: u64,
    client: Arc<McpClientHandle>,
    binding_leases: Option<Arc<super::apps::McpAppBindingLeaseRegistry>>,
    admission: Option<super::dynamic::admission::DynamicMcpAdmissionGate>,
}

const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINES: usize = 2000;

/// Sanitize name components to match API tool name pattern: ^[a-zA-Z0-9_-]+$
pub(crate) fn sanitize_name_component(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn app_allowed_tools(
    server_name: &str,
    resource_uri: &str,
    tools: &[Tool],
    dispatcher: &dyn peri_acp_types::tools::EffectiveToolDispatcher,
) -> std::collections::HashMap<String, String> {
    let dispatcher_tools = dispatcher
        .tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<std::collections::HashSet<_>>();
    tools
        .iter()
        .filter(|tool| {
            super::apps::tool_visibility(tool).app
                && super::apps::tool_resource_uri(tool).as_deref() == Some(resource_uri)
        })
        .filter_map(|tool| {
            let name = tool.name.to_string();
            let effective = effective_mcp_tool_name(server_name, &name);
            dispatcher_tools
                .contains(&effective)
                .then_some((name, effective))
        })
        .collect()
}

pub(crate) fn effective_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_name_component(server_name),
        sanitize_name_component(tool_name)
    )
}

impl McpToolBridge {
    pub fn new(server_name: &str, tool: &Tool, client: Arc<McpClientHandle>) -> Self {
        let tool_name = tool.name.to_string();
        let full_name = effective_mcp_tool_name(server_name, &tool_name);
        let description = format!(
            "[MCP:{}] {}",
            server_name,
            tool.description.as_ref().map(|d| d.as_ref()).unwrap_or("")
        );
        let input_schema = serde_json::to_value(&*tool.input_schema)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        Self {
            server_name: server_name.to_string(),
            tool_name,
            full_name,
            description,
            input_schema,
            model_visible: super::apps::tool_visibility(tool).model,
            direct: false,
            server_generation: 0,
            client,
            binding_leases: None,
            admission: None,
        }
    }

    pub fn new_dynamic(
        server_name: &str,
        tool: &Tool,
        client: Arc<McpClientHandle>,
        admission: super::dynamic::admission::DynamicMcpAdmissionGate,
    ) -> Result<Self, ToolCallError> {
        let tool_name = tool.name.to_string();
        if !valid_name_component(server_name) || !valid_name_component(&tool_name) {
            return Err(ToolCallError::Unavailable {
                server: server_name.to_string(),
            });
        }
        let description = format!(
            "[MCP:{}] {}",
            server_name,
            tool.description
                .as_ref()
                .map(|value| value.as_ref())
                .unwrap_or("")
        );
        Ok(Self {
            server_name: server_name.to_string(),
            full_name: effective_mcp_tool_name(server_name, &tool_name),
            tool_name,
            description,
            input_schema: serde_json::to_value(&*tool.input_schema)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            model_visible: super::apps::tool_visibility(tool).model,
            direct: false,
            server_generation: 0,
            client,
            binding_leases: None,
            admission: Some(admission),
        })
    }

    pub fn with_server_generation(mut self, generation: u64) -> Self {
        self.server_generation = generation;
        self
    }

    pub fn with_binding_leases(
        mut self,
        registry: Arc<super::apps::McpAppBindingLeaseRegistry>,
    ) -> Self {
        self.binding_leases = Some(registry);
        self
    }

    /// 将本 bridge 提升为 direct（无需模型先搜索即可出现在 tools 参数中）。
    ///
    /// 只改 direct 标记：visibility、名称、client、generation、admission 与
    /// binding leases 均不变，也不产生副本或新注册。
    ///
    /// 调用点归 `system_tools::prepare_system_tools`（同 Wave 落地）；
    /// 接线前显式豁免未使用告警。
    #[allow(dead_code)]
    pub(crate) fn with_direct(mut self) -> Self {
        self.direct = true;
        self
    }

    /// MCP 声明的原始工具名（未净化、未加 server 前缀）。
    ///
    /// effective name 的净化不可逆（分隔符与 `__` 都可能出现在分量内），
    /// 需要按原始名匹配时必须经此访问器，不得反拆 `name()`。
    /// server identity 用 [`BaseTool::mcp_server_name`]。
    ///
    /// 调用点归 `system_tools::prepare_system_tools`（同 Wave 落地）；
    /// 接线前显式豁免未使用告警。
    #[allow(dead_code)]
    pub(crate) fn original_tool_name(&self) -> &str {
        &self.tool_name
    }
}

fn valid_name_component(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[async_trait]
impl BaseTool for McpToolBridge {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    fn mcp_server_name(&self) -> Option<&str> {
        Some(&self.server_name)
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    fn visible_to_model(&self) -> bool {
        self.model_visible
    }

    fn is_direct(&self) -> bool {
        self.direct
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let _permit = match &self.admission {
            Some(gate) => Some(gate.try_acquire().map_err(|_| {
                Box::new(ToolCallError::Unavailable {
                    server: self.server_name.clone(),
                }) as Box<dyn std::error::Error + Send + Sync>
            })?),
            None => None,
        };
        // 1. 检查连接状态
        match &self.client.peer {
            Some(_) => {}
            None => {
                return Err(Box::new(ToolCallError::NotConnected {
                    server: self.server_name.clone(),
                    status: format!("{:?}", self.client.status),
                }));
            }
        }

        let peer = self.client.peer.as_ref().unwrap();

        // 2. 构建 rmcp 请求参数
        let arguments = input.as_object().cloned().unwrap_or_default();
        let request = rmcp::model::CallToolRequestParams::new(self.tool_name.clone())
            .with_arguments(arguments);

        // 3. 带超时调用 peer.call_tool()
        let result = tokio::time::timeout(TOOL_CALL_TIMEOUT, peer.call_tool(request))
            .await
            .map_err(|_| ToolCallError::Timeout {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                timeout_secs: TOOL_CALL_TIMEOUT.as_secs(),
            })?
            .map_err(|e| ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason: e.to_string(),
            })?;

        // 4. 处理 is_error 标志。失败的实例化调用不得签发 App lease。
        if result.is_error.unwrap_or(false) {
            let error_text = format_contents(&result.content);
            let lines: Vec<&str> = error_text.lines().collect();
            let reason = if lines.len() > MAX_MCP_LINES {
                let persist_hint = persist_truncated_output(&error_text);
                let truncated: String = lines[..MAX_MCP_LINES].join("\n");
                format!(
                    "{truncated}\n\n[MCP error output truncated: {} total lines]{persist_hint}",
                    lines.len()
                )
            } else {
                error_text
            };
            return Err(Box::new(ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason,
            }));
        }

        if let (
            Some(registry),
            Some(dispatcher),
            Some(session_id),
            Some(turn_generation),
            Some(invocation_id),
        ) = (
            self.binding_leases.as_ref(),
            ctx.effective_tool_dispatcher.clone(),
            ctx.session_id.clone(),
            ctx.turn_generation.clone(),
            ctx.invocation_id.clone(),
        ) {
            if invocation_id.starts_with("mcp-app:") {
                if let Ok(raw_result) = serde_json::to_value(&result)
                    .and_then(serde_json::from_value::<peri_acp_types::mcp_apps::RawCallToolResult>)
                {
                    registry.record_raw_result(
                        &invocation_id,
                        serde_json::to_value(raw_result).unwrap_or(serde_json::Value::Null),
                    );
                }
            } else if let Some(resource_uri) = self
                .client
                .tools
                .iter()
                .find(|tool| tool.name.as_ref() == self.tool_name)
                .filter(|tool| super::apps::tool_visibility(tool).app)
                .and_then(super::apps::tool_resource_uri)
            {
                let allowed_tools = app_allowed_tools(
                    &self.server_name,
                    &resource_uri,
                    &self.client.tools,
                    dispatcher.as_ref(),
                );
                registry.issue(super::apps::McpAppBindingLease::new(
                    session_id,
                    turn_generation,
                    self.server_name.clone(),
                    self.server_generation,
                    resource_uri,
                    self.tool_name.clone(),
                    invocation_id,
                    allowed_tools,
                    dispatcher,
                    ctx.cancellation.clone(),
                ));
            }
        }

        // 5. 格式化返回（截断超大输出）
        let formatted = format_contents(&result.content);
        let lines: Vec<&str> = formatted.lines().collect();
        let output = if lines.len() > MAX_MCP_LINES {
            let persist_hint = persist_truncated_output(&formatted);
            let truncated: String = lines[..MAX_MCP_LINES].join("\n");
            format!(
                "{truncated}\n\n[MCP output truncated: {} total lines]{persist_hint}",
                lines.len()
            )
        } else {
            formatted
        };
        Ok(output)
    }
}

/// 将 content 列表格式化为纯文本字符串
fn format_contents(contents: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for content in contents {
        match content {
            rmcp::model::ContentBlock::Text(text_content) => {
                parts.push(text_content.text.clone());
            }
            rmcp::model::ContentBlock::Image(image_content) => {
                parts.push(format!("[image: {}]", image_content.mime_type));
            }
            rmcp::model::ContentBlock::Resource(embedded) => {
                let uri = match &embedded.resource {
                    rmcp::model::ResourceContents::TextResourceContents { uri, .. } => uri.clone(),
                    rmcp::model::ResourceContents::BlobResourceContents { uri, .. } => uri.clone(),
                    _ => "unknown".to_string(),
                };
                parts.push(format!("[resource: {}]", uri));
            }
            rmcp::model::ContentBlock::Audio(audio_content) => {
                parts.push(format!("[audio: {}]", audio_content.mime_type));
            }
            rmcp::model::ContentBlock::ResourceLink(link) => {
                parts.push(format!("[resource_link: {}]", link.uri));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// 从 McpClientPool 的所有已连接客户端中批量创建 typed McpToolBridge。
///
/// `build_tool_bridges` 的 typed 版本：调用方可以在同一批对象上做分类
/// （如 [`McpToolBridge::with_direct`]）后只装箱一次，避免同一工具被注册两份。
/// 两种 constructor、generation 与 binding leases 行为与原实现一致。
///
/// **IF-D13 生效点**：注册表中**声明为 direct** 的 builtin 工具在这里直接
/// `.with_direct()`（`direct = 声明的 direct || 启动期 system_mcp_tools 提升`，两者由
/// 「声明 direct 集合 == `system_mcp_tools` 集合」的断言锁死，不得冲突）。其余一律
/// 保持 deferred。判定只走 `builtin::is_declared_direct`（未实现 / 未知名恒 false），
/// 不在本文件硬编码任何 `mcp__*` 字面量或实例名。
pub(crate) fn build_typed_tool_bridges(pool: &McpClientPool) -> Vec<McpToolBridge> {
    build_bridges(pool, true)
}

/// 强制 deferred 的 typed 版本：public [`build_tool_bridges`] 专用。
///
/// 与 [`build_typed_tool_bridges`] 的唯一差异是不应用声明 direct，因此 public builder
/// 在 builtin 与外部 server 两种输入下的行为都与提取出 typed builder 之前**逐位一致**。
pub(crate) fn build_deferred_tool_bridges(pool: &McpClientPool) -> Vec<McpToolBridge> {
    build_bridges(pool, false)
}

fn build_bridges(pool: &McpClientPool, apply_declared_direct: bool) -> Vec<McpToolBridge> {
    let mut bridges: Vec<McpToolBridge> = Vec::new();
    for client in pool.get_all_clients() {
        let generation = pool.handle_generation(&client);
        for tool in &client.tools {
            let mut bridge = McpToolBridge::new(&client.name, tool, Arc::clone(&client))
                .with_server_generation(generation)
                .with_binding_leases(Arc::clone(&pool.app_binding_leases));
            if apply_declared_direct
                && super::builtin::is_declared_direct(&client.name, tool.name.as_ref())
            {
                bridge = bridge.with_direct();
            }
            bridges.push(bridge);
        }
    }
    bridges
}

/// 从 McpClientPool 的所有已连接客户端中批量创建 McpToolBridge
///
/// 全部返回值保持 deferred 默认行为（`is_direct() == false`）——包括 builtin 实例的
/// 工具：未类型化的 public builder 不参与 direct 提升（IF-D13），保持既有契约不变。
pub fn build_tool_bridges(pool: &McpClientPool) -> Vec<Box<dyn BaseTool>> {
    build_deferred_tool_bridges(pool)
        .into_iter()
        .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
        .collect()
}

/// 统一工具池组装：内置工具优先去重

#[cfg(test)]
#[path = "tool_bridge_test.rs"]
mod tests;

/// C-INJ-01 focused 回归：typed bridge 的 direct 提升不改变 bridge 身份，
/// 且既有 public `build_tool_bridges` 的 deferred 默认行为不变。
#[cfg(test)]
mod direct_flag_tests {
    use super::*;
    use crate::mcp::client::ClientStatus;

    fn make_tool(tool_name: &str) -> Tool {
        serde_json::from_value(serde_json::json!({
            "name": tool_name,
            "description": "Read a file",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }
        }))
        .unwrap()
    }

    fn make_handle(server: &str, tools: Vec<Tool>, status: ClientStatus) -> Arc<McpClientHandle> {
        Arc::new(McpClientHandle {
            name: server.to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools,
            resources: vec![],
            status,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        })
    }

    #[test]
    fn test_system_direct_flag_preserves_bridge_identity() {
        let bridge = McpToolBridge::new(
            "workspace",
            &make_tool("Read"),
            make_handle("workspace", vec![], ClientStatus::Disconnected),
        );
        assert!(!bridge.is_direct(), "new 缺省必须为 deferred");
        let name = bridge.name().to_string();
        let parameters = bridge.parameters();
        assert!(bridge.visible_to_model());

        let promoted = bridge.with_direct();
        assert!(promoted.is_direct());
        assert_eq!(promoted.name(), name);
        assert_eq!(promoted.original_tool_name(), "Read");
        assert_eq!(promoted.mcp_server_name(), Some("workspace"));
        assert_eq!(promoted.parameters(), parameters);
        assert!(
            promoted.visible_to_model(),
            "direct 不等于绕过 model visibility"
        );
    }

    #[test]
    fn test_build_tool_bridges_keeps_deferred_default_and_matches_typed() {
        let pool = McpClientPool::new_pending();
        let handle = make_handle(
            "workspace",
            vec![make_tool("Read")],
            ClientStatus::Connected,
        );
        pool.clients
            .write()
            .insert("workspace".to_string(), Arc::clone(&handle));

        let typed = build_typed_tool_bridges(&pool);
        let boxed = build_tool_bridges(&pool);
        assert_eq!(typed.len(), 1);
        assert_eq!(boxed.len(), typed.len());
        assert_eq!(boxed[0].name(), typed[0].name());
        assert!(!typed[0].is_direct(), "typed builder 缺省必须为 deferred");
        assert!(
            !boxed[0].is_direct(),
            "public builder 必须保持 deferred 默认"
        );
        // 既有 generation / binding leases 传递行为不得因提取 typed builder 而丢失
        assert_eq!(typed[0].server_generation, pool.handle_generation(&handle));
        assert!(typed[0].binding_leases.is_some());
    }
}
