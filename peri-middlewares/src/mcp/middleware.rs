use peri_agent::middleware::capabilities as hook_state;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::{
    agent::AgentCancellationToken,
    error::AgentError,
    middleware::r#trait::Middleware,
    session::{
        tool_catalog::{StartupRequiredTool, StartupToolUpdate},
        MessageKind, MessageSource as QueueMessageSource, QueuedMessage,
    },
    tools::BaseTool,
};
use serde_json::json;

use super::{
    client::{
        redact_mcp_error, ClientStatus, McpClientPool, NegotiatedSystemMcp, SystemMcpManifest,
        SystemReadinessError,
    },
    discover_tool::DiscoverMCPTool,
    resource_tool::McpResourceTool,
    system_tools::{prepare_system_tools, SystemToolError},
    tool_bridge::{build_typed_tool_bridges, McpToolBridge},
};

/// 启动准入错误文案的展示上限（字符）。固定模板本身远短于此；该上限只约束
/// 由 MCP 声明（server / tool 名）撑长的部分。
const MAX_STARTUP_REASON_CHARS: usize = 512;

/// 用户可见启动错误文本的最后一道清洗：控制字符折叠为空格、URL query 与凭据
/// 形态遮蔽、限长。
///
/// ACP 不会替任意 MCP cause 自动脱敏（`AgentError::user_facing_message` 走
/// `Display`），因此清洗必须在 MCP 边界完成；只保留阶段与安全类别，不输出
/// env / headers / URL 认证信息 / 协议 payload / schema 默认值。
fn safe_startup_reason(raw: &str) -> String {
    let folded: String = raw
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    redact_mcp_error(&folded)
        .chars()
        .take(MAX_STARTUP_REASON_CHARS)
        .collect()
}

/// 本次 System MCP 准入的候选快照（冻结签名：IF-M3 / sub-plan B §4.3）。
///
/// `bridges` 是**整批**静态 MCP bridge（必需项已提升 direct、其余保持
/// deferred）。它只在本次 `before_react_start` 内存在并经 `StartupState` 提交，
/// 不落 middleware 字段、不跨 loop 复用。
pub(crate) struct SystemReadySnapshot {
    pub negotiated: Vec<NegotiatedSystemMcp>,
    pub bridges: Vec<McpToolBridge>,
    /// 本次准入时关闭的 builtin 实例（IF-D10 面①）。
    ///
    /// 关闭必须在**同一次 turn 的同一份 frozen policy** 下过滤：`bridges` 已按它
    /// 过滤，`required_tools` 也必须按它过滤——否则被有意关闭的实例会被目录提交
    /// 判定为 `RequiredToolUnavailable`（有意的关闭被误报成启动失败）。
    /// `negotiated` 保持原样：readiness 仍是 pool 级事实（第 3 条语义分层）。
    pub closed_instances: BTreeSet<String>,
}

impl SystemReadySnapshot {
    /// 无 System 依赖时不产生 startup update（普通 MCP 的 pending/failed 不阻塞）。
    fn has_system_dependency(&self) -> bool {
        !self.negotiated.is_empty()
    }

    /// 必需工具身份（原始名 + effective name），供目录提交后复核「可直达」。
    ///
    /// `prepare_system_tools` 成功后每个必需项在整批 bridge 中恰好命中一次；
    /// 缺失只能来自并发换代，按 fail-closed 返回错误，不发布 ready。
    /// 关闭实例的必需项**不进入** required（与 `bridges` 的过滤同源）。
    fn required_tools(&self) -> Result<Vec<StartupRequiredTool>, SystemToolError> {
        let mut required: Vec<StartupRequiredTool> = Vec::new();
        for item in &self.negotiated {
            if super::builtin::is_closed(&item.requirement.server, &self.closed_instances) {
                continue;
            }
            for tool in &item.requirement.required_tools {
                let already = required.iter().any(|entry| {
                    entry.server_name == item.requirement.server
                        && entry.original_tool_name == *tool
                });
                if already {
                    // 重复配置幂等：不产生第二份注册。
                    continue;
                }
                let bridge = self.bridges.iter().find(|bridge| {
                    bridge.mcp_server_name() == Some(item.requirement.server.as_str())
                        && bridge.original_tool_name() == tool
                });
                let Some(bridge) = bridge else {
                    return Err(SystemToolError::MissingTool {
                        server: item.requirement.server.clone(),
                        tool: tool.clone(),
                    });
                };
                required.push(StartupRequiredTool {
                    server_name: item.requirement.server.clone(),
                    original_tool_name: tool.clone(),
                    effective_tool_name: bridge.name().to_string(),
                });
            }
        }
        Ok(required)
    }
}

/// MCP 中间件 —— 将所有已连接 MCP 服务器的工具和资源注入 ReAct 循环，
/// 并向模型通报 MCP 连接状态（首 turn 概览 + 运行中上下线变化）。
pub struct McpMiddleware {
    /// Session-projected MCP view used by resources, discovery, and status reporting.
    pool: Arc<McpClientPool>,
    /// Deployment-owned pool used to build static MCP tool bridges. Static bridges must retain
    /// deployment identity such as handle generations and MCP Apps binding leases; dynamic
    /// tools are overlaid separately by `SessionToolCatalog`.
    tool_pool: Arc<McpClientPool>,
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
    /// 本 turn 关闭的 builtin 实例名集合（IF-D10 面①/②）。
    ///
    /// 来源 = 通话装配面按冻结的 `meta_harness_disabled` 求出的
    /// `builtin::closed_instances`；空集合 = 不关闭任何实例（既有测试与
    /// print 模式语义逐位不变）。判定只走 `builtin::is_closed`，
    /// 本文件不硬编码实例名或 `mcp__web__` 前缀。
    builtin_closures: BTreeSet<String>,
}

impl McpMiddleware {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            tool_pool: Arc::clone(&pool),
            pool,
            registry: None,
            command_registry: None,
            cancel: AgentCancellationToken::new(),
            hint_sent: AtomicBool::new(false),
            builtin_closures: BTreeSet::new(),
        }
    }

    /// Use a deployment-owned pool for static tool bridges while retaining the session-projected
    /// pool for resources and discovery.
    pub fn with_tool_pool(mut self, tool_pool: Arc<McpClientPool>) -> Self {
        self.tool_pool = tool_pool;
        self
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

    /// 注入本次 turn 的 builtin 关闭集（IF-D10 面①/②；装配面按冻结的
    /// `meta_harness_disabled` 用 `builtin::closed_instances` 求出）。
    ///
    /// 语义分层（IF-D10 第 3 条）：关闭集**只**决定本 turn 是否投影该实例的工具，
    /// 不影响 readiness——`await_system_connections` 仍按 pool 级配置判定实例
    /// 是否 ready（「实例必须健康」与「本 turn 是否注入」是两件事），
    /// 因此有意的关闭不会被误报成启动失败。
    pub(crate) fn with_builtin_closures(mut self, closed: BTreeSet<String>) -> Self {
        self.builtin_closures = closed;
        self
    }

    /// 该 bridge 所属实例是否在本 turn 被关闭（唯一判定入口 `builtin::is_closed`）。
    fn is_bridge_closed(&self, bridge: &McpToolBridge) -> bool {
        bridge
            .mcp_server_name()
            .is_some_and(|server| super::builtin::is_closed(server, &self.builtin_closures))
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
        let cache = pool
            .persistent_cache_allowed(&handle.name)
            .then(|| (pool.resource_cache(), pool.cache_origin(&handle.name)));
        let reg = Arc::clone(registry);
        let cmd_reg = command_registry.cloned();
        let cancel = cancel.clone();
        runtime.spawn(async move {
            crate::mcp::skill_discovery::run_discovery_with_cache(
                reg,
                cmd_reg,
                handle,
                handle_token,
                cancel,
                cache,
            )
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
    let discovery_pool = Arc::downgrade(pool);
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
            let Some(discovery_pool) = discovery_pool.upgrade() else {
                return;
            };
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
    /// System MCP 启动闸门（冻结签名：IF-M3 / sub-plan B §4.3）。
    ///
    /// 执行顺序：本次入场统一计时 → 等待 transport / initialize / 能力协商 /
    /// live `tools/list` → 一次 typed 构建整批静态 bridge →
    /// [`prepare_system_tools`] 逐项解析与校验 → 提交前复核（取消 / pool 开闭 /
    /// 代际 / deadline）。
    ///
    /// 返回**待目录提交的 candidate**：本函数不发布 ready、不改写 catalog。任何
    /// 失败都返回类型化错误，不 `warn` 后继续、不返回空集合、不产生部分结果。
    pub(crate) async fn await_system_ready(
        &self,
    ) -> Result<SystemReadySnapshot, SystemReadinessError> {
        // 1R 入场即计时：initialize / list / 必需工具校验之间不重置 deadline，
        // 多台 server 并发计时，不串行相加。
        let started_at = tokio::time::Instant::now();
        let negotiated = self
            .tool_pool
            .await_system_connections(&self.cancel, started_at)
            .await?;
        if negotiated.is_empty() {
            // 无 System 依赖：不产生 startup update；普通工具走原收集路径。
            return Ok(SystemReadySnapshot {
                negotiated,
                bridges: Vec::new(),
                closed_instances: self.builtin_closures.clone(),
            });
        }
        // 必需工具来自本次协商的 requirement（含空数组：该 server 只要求 ready）。
        // 静态 bridge 一律取 deployment `tool_pool`，不用会混入动态投影的 session
        // projection。
        //
        // 关闭实例（IF-D10 面①）既不进 required、也不进 bridges：有意的关闭不得
        // 变成 `RequiredToolUnavailable` fatal，同时其工具不得被提升为 direct。
        // readiness 判定（`await_system_connections`）不受影响——它走 pool 级配置。
        let required: BTreeMap<String, Vec<String>> = negotiated
            .iter()
            .filter(|item| {
                !super::builtin::is_closed(&item.requirement.server, &self.builtin_closures)
            })
            .map(|item| {
                (
                    item.requirement.server.clone(),
                    item.requirement.required_tools.clone(),
                )
            })
            .collect();
        let typed = self.open_typed_bridges();
        let bridges = prepare_system_tools(typed, &required)
            .map_err(|source| SystemReadinessError::RequiredTools { source })?;
        // `prepare_system_tools` 是同步校验，不 yield：返回后必须重新核对代际 /
        // pool 开闭 / 取消 / deadline，避免用旧代快照发布 ready。
        self.recheck_system_snapshot(&negotiated, started_at)?;
        Ok(SystemReadySnapshot {
            negotiated,
            bridges,
            closed_instances: self.builtin_closures.clone(),
        })
    }

    /// 本次 turn 的 typed bridge 集合：类型化构造（声明的 direct 生效，IF-D13）
    /// + **关闭集过滤**（IF-D10 面①/②）。
    ///
    /// 关闭实例的 bridge 既不得进入 deferred 目录（面②），也不得被提升为
    /// direct（面①）；过滤只走 `closed_instances` / `is_closed`，不硬编码实例名。
    fn open_typed_bridges(&self) -> Vec<McpToolBridge> {
        build_typed_tool_bridges(&self.tool_pool)
            .into_iter()
            .filter(|bridge| !self.is_bridge_closed(bridge))
            .collect()
    }

    /// 提交前复核：任何一项不成立都不得发布 ready。
    fn recheck_system_snapshot(
        &self,
        negotiated: &[NegotiatedSystemMcp],
        started_at: tokio::time::Instant,
    ) -> Result<(), SystemReadinessError> {
        if self.cancel.is_cancelled() {
            return Err(SystemReadinessError::Cancelled);
        }
        if !self.tool_pool.is_open() {
            return Err(SystemReadinessError::PoolClosed);
        }
        let now = tokio::time::Instant::now();
        for item in negotiated {
            let server = item.requirement.server.as_str();
            let current = self
                .tool_pool
                .get_client(server)
                .map(|handle| self.tool_pool.handle_generation(&handle));
            if current != Some(item.generation) {
                return Err(SystemReadinessError::ConnectionChanged {
                    server: server.to_string(),
                });
            }
            if now >= started_at + item.requirement.timeout {
                return Err(SystemReadinessError::Timeout {
                    server: server.to_string(),
                    timeout_ms: u64::try_from(item.requirement.timeout.as_millis())
                        .unwrap_or(u64::MAX),
                });
            }
        }
        Ok(())
    }

    /// 候选 → 目录提交 DTO；无 System 依赖时返回 `None`（不产生 startup update）。
    fn startup_tool_update(
        &self,
        snapshot: &SystemReadySnapshot,
    ) -> Result<Option<StartupToolUpdate>, SystemToolError> {
        if !snapshot.has_system_dependency() {
            return Ok(None);
        }
        Ok(Some(StartupToolUpdate {
            tools: snapshot
                .bridges
                .iter()
                .cloned()
                .map(|bridge| Arc::new(bridge) as Arc<dyn BaseTool>)
                .collect::<Vec<Arc<dyn BaseTool>>>(),
            required: snapshot.required_tools()?,
        }))
    }

    /// 闸门错误 → Agent 边界错误（IF-M3 两条硬约束 + 安全文案）。
    ///
    /// `Cancelled` → `Interrupted`（取消不是失败，不算 fatal）；**timeout 不是
    /// 取消**，与其它变体一起映射 fatal `MiddlewareError`，reason 为固定安全文案。
    fn startup_agent_error(&self, error: SystemReadinessError) -> AgentError {
        match error.into_agent_error(self.name()) {
            AgentError::MiddlewareError { middleware, reason } => AgentError::MiddlewareError {
                middleware,
                reason: safe_startup_reason(&reason),
            },
            other => other,
        }
    }

    /// 本批收集的静态 MCP bridge 集合。
    ///
    /// System 依赖已具备可信配置清单时使用 [`prepare_system_tools`] 的**整批**
    /// 结果（必需项 direct、普通 deferred）整体替换 deferred 收集，禁止在旧集合上
    /// 再 append 一份所需工具。准入候选本身不落 middleware 字段（IF-M5）：这里用与
    /// 闸门同一套纯函数按当次 handle 快照推导，不跨 loop / 跨 session 复用旧代标记。
    ///
    /// 两条路径都走**类型化构造 + 关闭集过滤**（IF-D10 面①/②，A6/R22）：
    /// 关闭实例的 bridge 不进入目录，声明的 direct（IF-D13）对未关闭实例生效。
    ///
    /// 校验不通过（缺工具 / schema / 可见性 / 有效名碰撞）时退回既有 deferred
    /// 收集：该结果不构成 ready，闸门仍会在进入 Compact 前以 fatal 结束本次 loop。
    fn static_tool_bridges(&self) -> Vec<Box<dyn BaseTool>> {
        match self.prepared_static_bridges() {
            Some(prepared) => prepared
                .into_iter()
                .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
                .collect(),
            None => self
                .open_typed_bridges()
                .into_iter()
                .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
                .collect(),
        }
    }

    /// `Some(整批 prepared bridge)` 仅当配置清单已完整发布且必需工具校验通过。
    fn prepared_static_bridges(&self) -> Option<Vec<McpToolBridge>> {
        // 清单未发布（Pending / Failed）时 `configs` 不是可信的 System 依赖事实源。
        if self.tool_pool.system_manifest() != SystemMcpManifest::Loaded {
            return None;
        }
        let required: BTreeMap<String, Vec<String>> = self
            .tool_pool
            .system_requirements()
            .into_iter()
            // 关闭实例不参与必需工具校验（与 `await_system_ready` 同一份关闭集）。
            .filter(|requirement| {
                !super::builtin::is_closed(&requirement.server, &self.builtin_closures)
            })
            .map(|requirement| (requirement.server, requirement.required_tools))
            .collect();
        if required.is_empty() {
            // 无 System 依赖（含「全部 system 实例被关闭」）：prepared 与 deferred
            // 集合等价，保持原路径。
            return None;
        }
        prepare_system_tools(self.open_typed_bridges(), &required).ok()
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

    /// 状态变化以 canonical Info reminder 注入模型上下文。
    ///
    /// 首条推送附 tool search 提示（每个会话恰好一次），后续只推送变化行。
    fn push_status_changes(&self, state: &mut dyn hook_state::QueueState) {
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
            let reminder = TrustedSystemReminderFactory::for_producer()
                .construct(SystemReminder {
                    version: SYSTEM_REMINDER_VERSION,
                    category: ReminderCategory::Lifecycle,
                    source: ReminderSource("mcp".into()),
                    kind: "connection_status_changed".into(),
                    severity: if text.contains("failed") {
                        ReminderSeverity::Warning
                    } else {
                        ReminderSeverity::Info
                    },
                    delivery: ReminderDelivery::Configurable,
                    audiences: ReminderAudiences(vec![
                        ReminderAudience::Model,
                        ReminderAudience::Tui,
                        ReminderAudience::Diagnostics,
                    ]),
                    summary: Some(text.clone()),
                    body: text,
                    metadata: json!({}),
                })
                .expect("MCP status reminder mapping must be valid");
            queue.push(QueuedMessage::system_reminder(
                MessageKind::Info,
                QueueMessageSource::SystemInjected,
                reminder,
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
        // 整批替换初始 Vec（不是 append）：System 依赖就绪时 prepared 集合已包含
        // 全部静态 bridge（必需项 direct），再 extend 会重复注册同一工具。
        let mut tools = self.static_tool_bridges();

        tools.push(Box::new(McpResourceTool::new(
            Arc::clone(&self.pool),
            // 未装配 session 注册表（print 模式/既有测试）→ 空注册表：
            // 无条目 = 无内容绑定校验（与现状一致）。
            self.registry
                .clone()
                .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
        )));

        tools.push(Box::new(
            DiscoverMCPTool::new(Arc::clone(&self.pool), self.registry.clone())
                .with_agent_registry(Arc::new(super::agent_registry::McpAgentRegistry::new(
                    Arc::clone(&self.pool),
                ))),
        ));

        tools
    }

    /// 首轮用户 turn：注入 MCP 基础情况概览（覆盖"初始化已完成、无上下线
    /// 事件"的场景）。由 executor 在首 turn 组装前调用。
    async fn first_turn_reminder(
        &self,
        state: &mut dyn hook_state::QueueState,
    ) -> peri_agent::error::AgentResult<Option<String>> {
        let Some(body) = self.overview_text() else {
            return Ok(None);
        };
        let summary = body.lines().next().map(str::to_string);
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Capability,
                source: ReminderSource("mcp".into()),
                kind: "connection_summary".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Tui,
                    ReminderAudience::Diagnostics,
                ]),
                body,
                summary,
                metadata: json!({}),
            })
            .map_err(|error| peri_agent::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: error.to_string(),
            })?;
        state.enqueue_v2_message(QueuedMessage::system_reminder(
            MessageKind::Info,
            QueueMessageSource::SystemInjected,
            reminder,
        ));
        Ok(None)
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
        _state: &mut dyn hook_state::BeforeAgentState,
    ) -> peri_agent::error::AgentResult<()> {
        self.ensure_discovery();
        Ok(())
    }

    /// 启动闸门：System MCP 未完成 transport / initialize / 能力协商 / 必需工具
    /// 检查前不得进入 Compact / Reason / Act（契约 2）。
    ///
    /// - 无 System 依赖时零动作：普通 MCP 的 pending / failed 永不阻塞启动；
    /// - 候选经 `StartupState` 暂存，失败或取消时随本次 state 丢弃，不落 middleware
    ///   字段、不发布 ready、不写宿主共享工具表；
    /// - 既有 discovery 与状态通知行为不变（仍在 `before_agent` / `before_model`）。
    async fn before_react_start(
        &self,
        state: &mut dyn hook_state::StartupState,
    ) -> peri_agent::error::AgentResult<()> {
        let snapshot = self
            .await_system_ready()
            .await
            .map_err(|error| self.startup_agent_error(error))?;
        match self.startup_tool_update(&snapshot) {
            Ok(Some(update)) => state.stage_startup_tools(update),
            Ok(None) => Ok(()),
            Err(source) => {
                Err(self.startup_agent_error(SystemReadinessError::RequiredTools { source }))
            }
        }
    }

    /// 每轮 ReAct 迭代：drain 状态变化缓冲并以 Info 消息推送（不唤醒循环；
    /// 空闲期变化由下个 turn 首轮 Receive 消费）。
    async fn before_model(
        &self,
        state: &mut dyn hook_state::BeforeModelState,
    ) -> peri_agent::error::AgentResult<()> {
        self.push_status_changes(state);
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
