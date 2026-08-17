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

    /// 幂等发现驱动（决策 B）：装配后立即 / pool 连接完成事件 / before_agent
    /// 三挂点共用同一执行体。
    ///
    /// 幂等性由两侧注册表投影保证：`project_connected` / `project_sources`
    /// 只对「无状态或 handle 变化（`!Arc::ptr_eq`）」的来源返回
    /// `to_discover`——Started 去重、Completed 跳过、重连经 ptr_eq 重新
    /// 进入，重复调用安全（无新来源时零 spawn）。
    pub(crate) fn ensure_discovery(&self) {
        run_ensure_discovery(
            &self.pool,
            self.registry.as_ref(),
            self.command_registry.as_ref(),
            &self.cancel,
        );
    }
}

/// 发现驱动执行体（决策 B；[`McpMiddleware::ensure_discovery`] 与装配面
/// pool 连接完成钩子共用）。无 tokio runtime 时跳过 spawn（装配期测试等
/// 场景；before_agent 幂等兜底，不 panic）。
pub(crate) fn run_ensure_discovery(
    pool: &Arc<McpClientPool>,
    registry: Option<&Arc<McpSkillRegistry>>,
    command_registry: Option<&Arc<CommandRegistry>>,
    cancel: &AgentCancellationToken,
) {
    let Some(registry) = registry else {
        return;
    };
    if cancel.is_cancelled() {
        return;
    }
    let connected: Vec<(String, HandleToken)> = pool
        .get_all_clients()
        .into_iter()
        .map(|h| {
            let t: HandleToken = h.clone();
            (h.name.clone(), t)
        })
        .collect();
    // 命令面投影（决策 1）：同 connected 列表，来源键 =
    // `mcp_source_key(server)`（plugin server key 取末段，与
    // mcp_route_entries 的 fullname 词法首段同构——断连批量注销
    // `{末段}:` 才能命中条目）。
    if let Some(reg) = command_registry {
        // 审查 B1：保留词法域 server 整体跳过命令面（不 Started、断连
        // 不注销）——源头无键即不会误删内置域条目；元数据面照常。
        let cmd_connected: Vec<(String, HandleToken)> = connected
            .iter()
            .filter(|(name, _)| !crate::mcp::skill_discovery::mcp_namespace_reserved(name))
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
    let Some(runtime) = tokio::runtime::Handle::try_current().ok() else {
        // 无 tokio runtime（装配期/纯函数测试）：跳过 spawn，before_agent
        // 幂等兜底（生产路径恒在 runtime 内，不触发本分支）。
        return;
    };
    for (name, handle_token) in projection.to_discover {
        // 仅置位者 spawn（审查 M1）：装配后立即 / 连接完成事件 / before_agent
        // 三个挂点可并发执行，`mark_discovery_started` 返回 false（覆盖已有
        // Started）时跳过 spawn，防重复发现任务与命令面重复回写。
        if !registry.mark_discovery_started(&name, handle_token.clone()) {
            continue;
        }
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
        let Some(handle) = pool.get_client(&name) else {
            continue;
        };
        let reg = Arc::clone(registry);
        let cmd_reg = command_registry.cloned();
        let cancel = cancel.clone();
        runtime.spawn(async move {
            crate::mcp::skill_discovery::run_discovery(reg, cmd_reg, handle, handle_token, cancel)
                .await;
        });
    }
}

/// session/new 预热入口（决策 B 扩展，审查会话生命周期）：不装配 chain
/// 即可触发幂等发现——新会话（/clear）在首 turn 装配前即 spawn 发现，
/// 面板无需等首轮消息即有 mcp 命令。幂等语义与装配面一致（Started 去重 /
/// Completed 跳过 / 重连 ptr_eq）；已连接 server 立即发现，连接中的
/// server 空跑，由首 turn 装配与连接完成事件兜底。cancel 持调用方 session
/// token，session 关闭即早退。
pub fn prewarm_discovery(
    pool: &Arc<McpClientPool>,
    registry: &Arc<McpSkillRegistry>,
    command_registry: &Arc<CommandRegistry>,
    cancel: &AgentCancellationToken,
) {
    run_ensure_discovery(pool, Some(registry), Some(command_registry), cancel);
}

/// 挂接 pool 连接完成事件（决策 B）：Connected 状态变化 → 触发幂等发现，
/// 补偿「装配时连接尚未完成 / 重连 / OAuth 授权后连接」的场景。装配面
/// 与 session/new 预热面共用（覆盖语义：后挂者生效，持其 cancel 生命周期）。
///
/// `notify_tx`：装配面传入 session 事件通道以展示连接通知（SystemNotification）；
/// session/new 预热面无 ExecutorEvent 通道传 None（仅发现触发；首 turn 装配
/// 时覆盖为完整版，窗口期行为与 notifier 未挂一致，无退化）。
pub fn attach_connection_notifier(
    pool: &Arc<McpClientPool>,
    registry: Option<&Arc<McpSkillRegistry>>,
    command_registry: Option<&Arc<CommandRegistry>>,
    cancel: &AgentCancellationToken,
    notify_tx: Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>>,
) {
    let discovery_pool = Arc::clone(pool);
    let discovery_registry = registry.cloned();
    let discovery_cmd = command_registry.cloned();
    let discovery_cancel = cancel.clone();
    pool.set_notifier(Box::new(move |text: &str| {
        if let Some(tx) = notify_tx.as_ref() {
            let _ = tx.send(
                peri_agent::agent::events::ExecutorEvent::SystemNotification {
                    text: text.to_string(),
                    level: "info".to_string(),
                },
            );
        }
        // 文本匹配 Connected 固定形态（status_change_text 唯一来源，
        // `connected (` 后缀稳定；Failed reason 含 " connected " 不误触发）。
        if text.contains(" connected (") {
            run_ensure_discovery(
                &discovery_pool,
                discovery_registry.as_ref(),
                discovery_cmd.as_ref(),
                &discovery_cancel,
            );
        }
    }));
}

impl McpMiddleware {
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

    /// 每轮投映 pool 已连接 server → 触发 MCP skill 发现（决策 B：before_agent
    /// 保留为幂等增量挂点，装配后立即 / pool 连接完成事件共用同一执行体）。
    ///
    /// - registry 未装配 / cancel 已触发 → 直接返回（零动作）；
    /// - `project_connected` 内部完成断连清理（有移除才触发 on_change）；
    /// - 需发现的 (name, handle) 同步置 Started 后 spawn 发现任务（持
    ///   session cancel token）。发现本身静默：不向 state 写任何消息。
    /// - 命令面（`command_registry` 装配时）：同 connected 列表以
    ///   [`crate::mcp::skill_discovery::mcp_source_key`] 投影来源
    ///   （Started/断连清理），发现任务完成回写经
    ///   [`crate::mcp::skill_discovery::run_discovery`] 双写（元数据面 +
    ///   命令面）。
    async fn before_agent(
        &self,
        _state: &mut dyn MiddlewareState,
    ) -> peri_agent::error::AgentResult<()> {
        self.ensure_discovery();
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
