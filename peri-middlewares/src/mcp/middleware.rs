use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_agent::{
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};

use super::{
    client::{ClientStatus, McpClientPool},
    resource_tool::McpResourceTool,
    tool_bridge::build_tool_bridges,
};

/// MCP 中间件 —— 将所有已连接 MCP 服务器的工具和资源注入 ReAct 循环，
/// 并向模型通报 MCP 连接状态（首 turn 概览 + 运行中上下线变化）。
pub struct McpMiddleware {
    pool: Arc<McpClientPool>,
    /// 是否已向模型提示过 tool search 用法（每个会话实例恰好一次）
    hint_sent: AtomicBool,
}

impl McpMiddleware {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            hint_sent: AtomicBool::new(false),
        }
    }

    /// 首 turn 概览：MCP 基础情况（服务器名 + 状态 + 工具数），失败报名字 + 错误。
    ///
    /// 无任何已配置服务器时返回 `None`（零噪音，不注入）。
    fn overview_text(&self) -> Option<String> {
        let infos = self.pool.all_server_infos();
        if infos.is_empty() {
            return None;
        }
        let (mut connected, mut failed, mut disabled, mut other) = (0usize, 0usize, 0usize, 0usize);
        let mut lines = Vec::new();
        for info in &infos {
            match &info.status {
                ClientStatus::Connected => {
                    connected += 1;
                    lines.push(format!(
                        "- {} (connected, {} tools)",
                        info.name, info.tool_count
                    ));
                }
                ClientStatus::Failed(reason) => {
                    failed += 1;
                    lines.push(format!("- {} (failed: {})", info.name, reason));
                }
                ClientStatus::Disabled => {
                    disabled += 1;
                    lines.push(format!("- {} (disabled)", info.name));
                }
                ClientStatus::Disconnected => {
                    other += 1;
                    lines.push(format!("- {} (disconnected)", info.name));
                }
                ClientStatus::Uninitialized => {
                    other += 1;
                    lines.push(format!("- {} (uninitialized)", info.name));
                }
            }
        }
        let summary = format!("MCP: {connected} connected, {failed} failed, {disabled} disabled");
        if other > 0 {
            lines.push(format!("- {} 台未连接", other));
        }
        Some(format!(
            "{}\n{}\n\nMCP 工具经 tool search 发现并调用（格式 mcp__<server>__<tool>）。",
            summary,
            lines.join("\n")
        ))
    }

    /// 状态变化文本注入模型上下文（Info 消息，`<system-reminder>` 包裹）。
    ///
    /// 首条推送附 tool search 提示（每个会话恰好一次），后续只推送变化行。
    fn push_status_changes(&self, state: &mut dyn MiddlewareState) {
        let changes = self.pool.drain_pending_changes();
        if changes.is_empty() {
            return;
        }
        let queue = state.v2_queue();
        let mut texts = Vec::with_capacity(changes.len() + 1);
        if !self.hint_sent.swap(true, Ordering::SeqCst) {
            texts.push(
                "MCP 连接状态变化：MCP 工具经 tool search 发现并调用（格式 mcp__<server>__<tool>）。"
                    .to_string(),
            );
        }
        texts.extend(changes);
        for text in texts {
            queue.push(peri_agent::session::QueuedMessage::new(
                peri_agent::session::MessageKind::Info,
                peri_agent::session::MessageSource::SystemInjected,
                peri_agent::messages::BaseMessage::human(text),
            ));
        }
    }
}

#[async_trait]
impl Middleware for McpMiddleware {
    fn name(&self) -> &str {
        "McpMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        let mut tools = build_tool_bridges(&self.pool);

        if self.pool.has_resources() {
            tools.push(Box::new(McpResourceTool::new(Arc::clone(&self.pool))));
        }

        tools
    }

    /// 首轮用户 turn：注入 MCP 基础情况概览（覆盖"初始化已完成、无上下线
    /// 事件"的场景）。由 executor 在首 turn 组装前调用。
    async fn first_turn_reminder(
        &self,
        _state: &mut dyn MiddlewareState,
    ) -> peri_agent::error::AgentResult<Option<String>> {
        Ok(self.overview_text())
    }

    /// 每轮 ReAct 迭代：drain 状态变化缓冲并以 Info 消息推送（不唤醒循环；
    /// 空闲期变化由下个 turn 首轮 Receive 消费）。
    async fn before_model(
        &self,
        state: &mut dyn MiddlewareState,
    ) -> peri_agent::error::AgentResult<()> {
        self.push_status_changes(state);
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
