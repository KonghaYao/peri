//! Session-owned MCP 准入准备原语（readiness → candidate）。
//!
//! 本模块抽取自 [`super::middleware::McpMiddleware`] 的 `before_react_start` 前半段
//! （设计 §4.2：撤回「整体前移」，只抽**不依赖输入**的 readiness / candidate 准备
//! 原语）。全部输入都是 session 级事实——deployment tool pool、session id、取消
//! 令牌、关闭实例集——不读 turn 输入（消息、工具视图、catalog），因此 session 级
//! 持有者可以构造一次并跨 turn 复用；原 MCP startup hook 与后续 idle invocation
//! 消费**同一份**实现（业务规则单一权威，不复制第二份）。
//!
//! 与链序无关：本模块不决定何时准入（那仍由 `McpMiddleware` 的 hook 在既有槽位
//! 触发），也不提交 catalog——只把 session 级输入换算成待提交的 candidate。
//!
//! 边界（迁移自 middleware，语义逐字保留）：
//! - **不发布 ready、不改写 catalog**：只返回 candidate，提交由调用方的
//!   `StartupState` 完成；
//! - **不读用户输入**：readiness 全程不读取消息队列；
//! - 失败一律返回类型化错误（fail-closed），不 `warn` 后继续、不返回空集合、
//!   不产生部分结果；
//! - 复用 `system_tools::prepare_system_tools`（纯函数）与
//!   `McpClientPool::await_system_connections`（pool 级 readiness 事实）。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use peri_agent::{
    agent::AgentCancellationToken,
    error::AgentError,
    session::tool_catalog::{StartupRequiredTool, StartupToolUpdate},
    tools::BaseTool,
};

use super::{
    client::{McpClientPool, NegotiatedSystemMcp, SystemReadinessError},
    system_tools::{prepare_system_tools, SystemToolError},
    tool_bridge::{build_typed_tool_bridges_visible_to, McpToolBridge},
};

/// 启动准入错误归属的链槽名。
///
/// 与 [`super::middleware::McpMiddleware`] 的 `Middleware::name` 同源（唯一常量
/// 来源）：准入准备本身不是链槽，失败仍须归属到发起它的 `McpMiddleware`，
/// 用户可见的错误文案不因抽取而变化。
pub(crate) const MCP_MIDDLEWARE_NAME: &str = "McpMiddleware";

/// 启动准入错误文案的展示上限（字符）。固定模板本身远短于此；该上限只约束
/// 由 MCP 声明（server / tool 名）撑长的部分。
const MAX_STARTUP_REASON_CHARS: usize = 512;

/// 用户可见启动错误文本的控制字符折叠与长度限制。
fn safe_startup_reason(raw: &str) -> String {
    let folded: String = raw
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect();
    folded.chars().take(MAX_STARTUP_REASON_CHARS).collect()
}

/// 本次 System MCP 准入的候选快照（冻结签名：IF-M3 / sub-plan B §4.3）。
///
/// `bridges` 是**整批**静态 MCP bridge（必需项已提升 direct、其余保持
/// deferred）。它只在本次准入内存在并经 `StartupState` 提交，
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
    pub(crate) fn required_tools(&self) -> Result<Vec<StartupRequiredTool>, SystemToolError> {
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

/// session 级 MCP 准入准备：把 session 级输入换算成待提交的准入 candidate。
///
/// 无状态：不缓存上一轮结果、不持 turn 视图，同一实例可被同一 session 的多次准入
/// （idle invocation、每 turn 的 `before_react_start`）反复调用。
pub(crate) struct McpAdmissionPreparation {
    /// Deployment-owned pool：readiness 与静态 bridge 的事实源。
    ///
    /// 必须是 deployment pool 而不是 session projection——静态 bridge 要保留部署
    /// 身份（handle 代际、MCP Apps 绑定租约），动态投影由 `SessionToolCatalog`
    /// 另行叠加。
    tool_pool: Arc<McpClientPool>,
    /// ACP 归属过滤（`None` = 未装配会话上下文，不过滤）。
    session_id: Option<String>,
    /// session 取消令牌：取消 → `Interrupted`（不是失败），deadline 复核同源。
    cancel: AgentCancellationToken,
    /// 本 session 冻结策略下关闭的 builtin 实例集（空集 = 不关闭任何实例）。
    ///
    /// 来源 = 会话装配面按冻结的 `meta_harness_disabled` 用
    /// `builtin::closed_instances` 求出的集合。判定只走 `builtin::is_closed` /
    /// `is_closed_source`，本模块不硬编码实例名或 `mcp__web__` 前缀。
    closed_instances: BTreeSet<String>,
}

impl McpAdmissionPreparation {
    /// session 级构造入口：四个输入全是 session 级事实，其中没有 turn 输入。
    ///
    /// 调用方约定：
    /// - `tool_pool` 取 session pool；
    /// - `closed_instances` 取本 session **冻结**的 `meta_harness_disabled` 派生集
    ///   （`None` 语义不存在：空集即不关闭任何实例）；
    /// - `cancel` 与 session 同生命周期（取消后本次准入返回 `Cancelled`）。
    ///
    /// 目前唯一构造点是 `McpMiddleware::admission`（每 turn 新建的链实例按同一
    /// session 级输入投影一份）。将来 idle invocation 的 session 级持有者直接调用
    /// 本函数构造并长期持有；届时若持有者位于别的 crate，可见性一并放宽。
    pub(crate) fn new(
        tool_pool: Arc<McpClientPool>,
        session_id: Option<String>,
        cancel: AgentCancellationToken,
        closed_instances: BTreeSet<String>,
    ) -> Self {
        Self {
            tool_pool,
            session_id,
            cancel,
            closed_instances,
        }
    }

    /// 该 server 的实例是否在本 session 被关闭（唯一判定入口
    /// `builtin::is_closed_source`，与来源同源）。
    ///
    /// 供准入（required 过滤）与目录收集（`McpMiddleware::prepared_static_bridges`
    /// 的必需工具校验）共用同一份关闭集判定。
    pub(crate) fn is_source_closed(&self, server: &str) -> bool {
        let source = self
            .tool_pool
            .configs
            .read()
            .get(server)
            .and_then(|config| config.source.clone());
        super::builtin::is_closed_source(server, source.as_ref(), &self.closed_instances)
    }

    /// 该 bridge 所属实例是否在本 session 被关闭（唯一判定入口
    /// `builtin::is_closed`）。
    fn is_bridge_closed(&self, bridge: &McpToolBridge) -> bool {
        bridge
            .builtin_mcp_instance()
            .is_some_and(|server| super::builtin::is_closed(server, &self.closed_instances))
    }

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
        let started_at = peri_time::monotonic_now();
        let negotiated = self
            .tool_pool
            .await_system_connections(&self.cancel, started_at)
            .await?;
        if negotiated.is_empty() {
            // 无 System 依赖：不产生 startup update；普通工具走原收集路径。
            return Ok(SystemReadySnapshot {
                negotiated,
                bridges: Vec::new(),
                closed_instances: self.closed_instances.clone(),
            });
        }
        // 必需工具来自本次协商的 requirement（含空数组：该 server 只要求 ready）。
        // 工具 bridge 一律取本 session pool，保留连接身份与声明的
        // projection。
        //
        // 关闭实例（IF-D10 面①）既不进 required、也不进 bridges：有意的关闭不得
        // 变成 `RequiredToolUnavailable` fatal，同时其工具不得被提升为 direct。
        // readiness 判定（`await_system_connections`）不受影响——它走 pool 级配置。
        let required: BTreeMap<String, Vec<String>> = negotiated
            .iter()
            .filter(|item| !self.is_source_closed(&item.requirement.server))
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
            closed_instances: self.closed_instances.clone(),
        })
    }

    /// 本次准入的 typed bridge 集合：类型化构造（声明的 direct 生效，IF-D13）
    /// + **关闭集过滤**（IF-D10 面①/②）。
    ///
    /// 关闭实例的 bridge 既不得进入 deferred 目录（面②），也不得被提升为
    /// direct（面①）；过滤只走 `closed_instances` / `is_closed`，不硬编码实例名。
    pub(crate) fn open_typed_bridges(&self) -> Vec<McpToolBridge> {
        build_typed_tool_bridges_visible_to(&self.tool_pool, self.session_id.as_deref())
            .into_iter()
            .filter(|bridge| !self.is_bridge_closed(bridge))
            .collect()
    }

    /// 提交前复核：任何一项不成立都不得发布 ready。
    fn recheck_system_snapshot(
        &self,
        negotiated: &[NegotiatedSystemMcp],
        started_at: peri_time::Instant,
    ) -> Result<(), SystemReadinessError> {
        if self.cancel.is_cancelled() {
            return Err(SystemReadinessError::Cancelled);
        }
        if !self.tool_pool.is_open() {
            return Err(SystemReadinessError::PoolClosed);
        }
        let now = peri_time::monotonic_now();
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
    pub(crate) fn startup_tool_update(
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
    pub(crate) fn startup_agent_error(&self, error: SystemReadinessError) -> AgentError {
        match error.into_agent_error(MCP_MIDDLEWARE_NAME) {
            AgentError::MiddlewareError { middleware, reason } => AgentError::MiddlewareError {
                middleware,
                reason: safe_startup_reason(&reason),
            },
            other => other,
        }
    }
}
