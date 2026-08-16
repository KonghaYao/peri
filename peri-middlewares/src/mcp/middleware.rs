use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
use peri_agent::{
    agent::AgentCancellationToken,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};

use super::{
    client::{ClientStatus, McpClientPool},
    discover_tool::DiscoverMCPTool,
    resource_tool::McpResourceTool,
    tool_bridge::build_tool_bridges,
};

/// MCP 中间件 —— 将所有已连接 MCP 服务器的工具和资源注入 ReAct 循环，
/// 并向模型通报 MCP 连接状态（首 turn 概览 + 运行中上下线变化）。
pub struct McpMiddleware {
    pool: Arc<McpClientPool>,
    /// 会话级 MCP skill 远端注册表（None = 未装配 session 透传；DiscoverMCP
    /// 的 skill 域查询读它）。
    registry: Option<Arc<McpSkillRegistry>>,
    /// 会话级命令注册表（命令面，Phase 6 A3；None = 未装配 session 透传，
    /// 跳过 mcp 域命令发现投影）。与 `registry` 是两条独立写路径：
    /// 元数据面发现结果经 [`crate::mcp::skill_discovery::mcp_route_entries`]
    /// 转换后写本注册表。
    command_registry: Option<Arc<CommandRegistry>>,
    /// session 取消令牌（发现任务持有；触发后 before_agent 不再投影/spawn）
    cancel: AgentCancellationToken,
    /// 是否已向模型提示过 tool search 用法（每个会话实例恰好一次）
    hint_sent: AtomicBool,
}

impl McpMiddleware {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            registry: None,
            command_registry: None,
            cancel: AgentCancellationToken::new(),
            hint_sent: AtomicBool::new(false),
        }
    }

    /// 注入 skill 发现装配（session 级 registry + cancel token；assembly 槽位
    /// 调用）。不调用时保持无发现行为（既有测试/print 模式兼容）。
    pub fn with_skill_discovery(
        mut self,
        registry: Option<Arc<McpSkillRegistry>>,
        cancel: AgentCancellationToken,
    ) -> Self {
        self.registry = registry;
        self.cancel = cancel;
        self
    }

    /// 注入命令面注册表（session 级 CommandRegistry；assembly 槽位调用，
    /// Phase 6 A3）。None = 未装配命令面（print 模式/既有测试），发现任务
    /// 仅回写元数据面。
    pub fn with_command_registry(self, command_registry: Option<Arc<CommandRegistry>>) -> Self {
        Self {
            command_registry,
            ..self
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
            tools.push(Box::new(McpResourceTool::new(
                Arc::clone(&self.pool),
                // 未装配 session 注册表（print 模式/既有测试）→ 空注册表：
                // 无条目 = 无内容绑定校验（与现状一致）。
                self.registry
                    .clone()
                    .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
            )));
        }

        tools.push(Box::new(DiscoverMCPTool::new(
            Arc::clone(&self.pool),
            self.registry.clone(),
        )));

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

    /// 每轮投映 pool 已连接 server → 触发 MCP skill 发现（DD-2）。
    ///
    /// - registry 未装配 / cancel 已触发 → 直接返回（零动作）；
    /// - `project_connected` 内部完成断连清理（有移除才触发 on_change）；
    /// - 需发现的 (name, handle) 同步置 Started 后 spawn 发现任务（持
    ///   session cancel token）。发现本身静默：不向 state 写任何消息。
    /// - 命令面（`command_registry` 装配时）：同 connected 列表以
    ///   [`crate::mcp::skill_discovery::mcp_source_key`] 投影来源
    ///   （Started/断连清理），发现任务完成回写经
    ///   [`crate::mcp::skill_discovery::run_discovery`] 双写（元数据面 +
    ///   命令面，Phase 6 A3）。
    async fn before_agent(
        &self,
        _state: &mut dyn MiddlewareState,
    ) -> peri_agent::error::AgentResult<()> {
        let Some(registry) = self.registry.as_ref() else {
            return Ok(());
        };
        if self.cancel.is_cancelled() {
            return Ok(());
        }
        let connected: Vec<(String, HandleToken)> = self
            .pool
            .get_all_clients()
            .into_iter()
            .map(|h| {
                let t: HandleToken = h.clone();
                (h.name.clone(), t)
            })
            .collect();
        // 命令面投影（Phase 6 A3，P1-1 修复）：同 connected 列表，来源键 =
        // `mcp_source_key(server)`（plugin server key 取末段，与
        // mcp_route_entries 的 fullname namespace 段同构——断连批量注销
        // `mcp:{末段}:` 才能命中条目）。
        if let Some(reg) = self.command_registry.as_ref() {
            let cmd_connected: Vec<(String, HandleToken)> = connected
                .iter()
                .map(|(name, token)| {
                    (
                        crate::mcp::skill_discovery::mcp_source_key(name),
                        token.clone(),
                    )
                })
                .collect();
            let cmd_projection = reg.project_sources(&cmd_connected);
            // removed_any 已由注册表内部消费（on_change 触发决策，含断连
            // 批量注销），本层只需处理 to_discover（与元数据面 Projection
            // 同构，非漏处理）。
            for (prefix, handle_token) in cmd_projection.to_discover {
                reg.mark_source_started(&prefix, handle_token);
            }
        }
        let projection = registry.project_connected(&connected);
        for (name, handle_token) in projection.to_discover {
            registry.mark_discovery_started(&name, handle_token.clone());
            // mark 与取 handle 之间可能断连/重连，两者都自愈，无需显式补偿：
            // - get_client 返回 None（断连）：Started 残留由下轮 before_agent 的
            //   project_connected 移除清理（server 已不在 connected 列表）；
            // - get_client 返回新 Arc（重连）：Started 中仍是旧 token，自愈触发
            //   源是下轮 project_connected 的 token 不一致检测（新 handle 与
            //   Started 旧 token 的 Arc::ptr_eq 不相等）→ 重新 to_discover +
            //   重新 Started，触发重扫。旧发现任务的完成回写被
            //   mark_discovery_completed 的 Arc::ptr_eq 拒绝，但那只发生在
            //   "下轮已用新 token 重新 Started" 的交错下——ptr_eq 拒绝是防御
            //   （旧任务不得覆盖新状态），不是重扫触发源。
            let Some(handle) = self.pool.get_client(&name) else {
                continue;
            };
            let reg = Arc::clone(registry);
            let cmd_reg = self.command_registry.clone();
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                crate::mcp::skill_discovery::run_discovery(
                    reg,
                    cmd_reg,
                    handle,
                    handle_token,
                    cancel,
                )
                .await;
            });
        }
        Ok(())
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
