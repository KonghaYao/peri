use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};
use peri_resources::lsp::uri::path_to_uri;
use peri_resources::lsp::{
    config::{LspConfigFile, LspServerConfig},
    pool::LspServerPool,
};

use super::tool::LspTool;
use crate::tool_search::core_tools::{TOOL_EDIT, TOOL_WRITE};

pub struct LspMiddleware {
    pool: Arc<LspServerPool>,
}

impl LspMiddleware {
    pub fn new(root_uri: String, config: LspConfigFile) -> Self {
        let pool = Arc::new(LspServerPool::new(&root_uri, config));
        Self { pool }
    }

    /// 复用既有 pool 构造（会话级共享：H1 下服务器进程/initialized/诊断状态
    /// 跨 turn 存活；由装配面从 `LspPoolPort` downcast 还原后注入）。
    pub fn from_pool(pool: Arc<LspServerPool>) -> Self {
        Self { pool }
    }

    pub fn from_configs(root_uri: String, configs: Vec<LspServerConfig>) -> Self {
        let config = LspConfigFile {
            lsp_servers: configs.into_iter().map(|c| (c.name.clone(), c)).collect(),
        };
        Self::new(root_uri, config)
    }

    pub fn shared_pool(&self) -> Arc<LspServerPool> {
        Arc::clone(&self.pool)
    }
}

#[async_trait]
impl Middleware for LspMiddleware {
    fn name(&self) -> &str {
        "LspMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        if !self.pool.has_servers() {
            return Vec::new();
        }
        vec![Box::new(LspTool::new(Arc::clone(&self.pool)))]
    }

    async fn after_tool(
        &self,
        _state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
        _result: &ToolResult,
    ) -> AgentResult<()> {
        if tool_call.name != TOOL_WRITE && tool_call.name != TOOL_EDIT {
            return Ok(());
        }

        let file_path = match tool_call.input.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return Ok(()),
        };

        let server = match self.pool.server_for_file(&file_path) {
            Some(s) if s.is_ready() => s,
            _ => return Ok(()),
        };

        let uri = path_to_uri(Path::new(&file_path));
        let text = match tokio::fs::read_to_string(&file_path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(target: "lsp", file = %file_path, error = %e, "LSP 同步文件时读取失败");
                return Ok(());
            }
        };

        if let Err(e) = server.did_change(&uri, &text).await {
            tracing::debug!(target: "lsp", file = %file_path, error = %e, "LSP didChange 失败");
        }
        if let Err(e) = server.did_save(&uri).await {
            tracing::debug!(target: "lsp", file = %file_path, error = %e, "LSP didSave 失败");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
