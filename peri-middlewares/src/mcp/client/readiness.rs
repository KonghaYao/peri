//! System MCP 启动准入的连接证据、等待与类型化错误（主 plan IF-M3，owner B-01）。
//!
//! 本模块只回答一个问题：**本次入场（1R）时，配置声明的 System MCP 是否已完成
//! transport + rmcp lifecycle + 能力协商 + live `tools/list`**。返回的
//! [`NegotiatedSystemMcp`] 是「连接/协商完成」的证据，**不是** ready：ready 的
//! 线性化点在目录提交后的最终复核（B-03）。
//!
//! 判定纪律：
//! - 只有真实成功的生产路径提交的 [`DiscoveryEvidence`] 才算证据。空数组是
//!   `tools/list` 的成功结果；`Err`、超时、任务结束、旧缓存、光秃的
//!   `ClientStatus::Connected` 都不得代替发现证据。
//! - 配置清单未完整发布（[`SystemMcpManifest::Pending`]）时，**不得**把空
//!   `configs` 当成「无 System MCP」；加载失败必须显式成为 `Failed`。
//! - 等待有界：每个 server 的 deadline 都从调用方传入的入场时间起算（并发计时，
//!   不串行相加）。timeout 是终态失败，**不是**取消。
//! - 需要人判断的状态（Disabled / 需要授权）一律返回错误，不主动弹 OAuth、
//!   不自动重试、不跳过。
//! - 证据只描述提交时刻的事实；是否属于**当前代**由读取方核对，旧代 Arc 永远
//!   不被接受。

use std::{collections::HashMap, sync::Arc, time::Duration};

use peri_acp_types::plugin::McpServerConfig;
use peri_agent::agent::AgentCancellationToken;
use thiserror::Error;

use super::{ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus};
use crate::mcp::system_tools::SystemToolError;

/// 安全展示用的 server 标签上限（字符数）。
const MAX_SERVER_LABEL_CHARS: usize = 64;

/// server/tool 展示用清洗：控制字符折叠为空格、移除 URL query、遮蔽凭据形态，
/// 并限制长度。错误文案不携带 env / headers / URL 认证信息 / 协议 payload。
fn safe_server_label(raw: impl AsRef<str>) -> String {
    let collapsed: String = raw
        .as_ref()
        .chars()
        .take(MAX_SERVER_LABEL_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    super::redact_mcp_error(&collapsed)
}

/// System MCP 启动等待的配置清单状态。
///
/// `Pending` 是构造初值，**不**等价于「无 System MCP」：`run_initialize` 在
/// async 初始化内才装载 `configs`，空 map 不能作为「无依赖」的证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SystemMcpManifest {
    /// 完整配置清单（含空集合）尚未发布。
    #[default]
    Pending,
    /// 完整配置清单已发布；此刻起 `configs` 是可信的 System 依赖事实源。
    Loaded,
    /// 配置加载或校验失败：不得退化成「无 System MCP」。
    Failed,
}

/// 本代发现证据（主 plan IF-M3 冻结字段）。
///
/// 提交纪律（生产路径必须遵守）：
/// - `generation` 取自该 server **当前已提交句柄**的 `handle_generation`；`0`
///   表示句柄未经提交登记，读取方一律视为无效。
/// - 一次发现尝试**结束时**才提交：尝试进行中不提交（无证据 = 仍在进行）。
/// - `tools_list_ok` 只允许真实成功的 live `tools/list` 提交；空数组是成功结果，
///   `Err` / 超时不得置位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DiscoveryEvidence {
    pub generation: u64,
    pub initialize_ok: bool,
    pub tools_list_ok: bool,
}

impl DiscoveryEvidence {
    /// initialize 失败（transport / lifecycle / 能力协商）；`tools/list` 未执行。
    pub(crate) fn initialize_failed(generation: u64) -> Self {
        Self {
            generation,
            initialize_ok: false,
            tools_list_ok: false,
        }
    }

    /// initialize 成功但 live `tools/list` 失败：这不是「空清单」，也不是完成。
    pub(crate) fn discovery_failed(generation: u64) -> Self {
        Self {
            generation,
            initialize_ok: true,
            tools_list_ok: false,
        }
    }

    /// initialize 与 live `tools/list` 都成功（清单可以为空数组）。
    pub(crate) fn discovered(generation: u64) -> Self {
        Self {
            generation,
            initialize_ok: true,
            tools_list_ok: true,
        }
    }

    /// 是否构成本代的完整发现证据（`generation == 0` 永远不算）。
    pub(crate) fn is_complete(&self) -> bool {
        self.generation != 0 && self.initialize_ok && self.tools_list_ok
    }
}

/// System MCP 要求（来自已发布的配置清单）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemMcpRequirement {
    pub server: String,
    /// 在所属 server 的**原始工具名**上精确匹配的必需工具；空数组表示只要求 ready。
    pub required_tools: Vec<String>,
    /// 该 server 的启动等待上限（从入场时间起算）。
    pub timeout: Duration,
}

impl SystemMcpRequirement {
    fn timeout_ms(&self) -> u64 {
        u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)
    }
}

/// 本代协商完成的 System MCP（transport + lifecycle + 能力协商 + `tools/list`）。
///
/// `generation` 是 `handle` 在 pool 中的登记代际；调用方在后续任何异步步骤之后
/// 必须重新核对代际（`pool.handle_generation`）与 pool 开闭状态，不得把本结构
/// 当作长期有效的 ready。
#[derive(Clone)]
pub(crate) struct NegotiatedSystemMcp {
    pub requirement: SystemMcpRequirement,
    pub handle: Arc<McpClientHandle>,
    pub generation: u64,
}

impl std::fmt::Debug for NegotiatedSystemMcp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NegotiatedSystemMcp")
            .field("server", &self.requirement.server)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// System MCP 启动准入错误（变体全集冻结于 sub-plan B §4.4；主 plan IF-M3 追加两条
/// 硬约束：`Cancelled → AgentError::Interrupted`、timeout 不是 cancel）。
///
/// 全部变体的 Display 都是**固定模板**：不含 `ClientStatus::Failed` 原文、env、
/// headers、URL 认证信息或协议 payload；`{server}` 经 [`safe_server_label`] 清洗。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SystemReadinessError {
    #[error("System MCP 启动失败：配置清单在 30000ms 内未就绪")]
    ConfigurationUnavailable,
    #[error("System MCP 启动失败：配置加载或校验失败")]
    ConfigurationFailed,
    #[error("System MCP 启动失败：连接池正在关闭或已关闭")]
    PoolClosed,
    #[error(
        "System MCP \"{}\" 启动失败：服务器已禁用",
        safe_server_label(.server)
    )]
    Disabled { server: String },
    #[error(
        "System MCP \"{}\" 启动失败：需要完成授权",
        safe_server_label(.server)
    )]
    AuthorizationRequired { server: String },
    #[error(
        "System MCP \"{}\" 启动失败：transport 或协议初始化失败",
        safe_server_label(.server)
    )]
    ConnectionFailed { server: String },
    #[error(
        "System MCP \"{}\" 启动失败：缺少有效协议协商证据",
        safe_server_label(.server)
    )]
    NegotiationIncomplete { server: String },
    #[error(
        "System MCP \"{}\" 启动失败：tools/list 失败",
        safe_server_label(.server)
    )]
    ToolDiscoveryFailed { server: String },
    #[error(
        "System MCP \"{}\" 启动失败：连接代际已变化，请重试本次输入",
        safe_server_label(.server)
    )]
    ConnectionChanged { server: String },
    #[error(
        "System MCP \"{}\" 启动超时（{}ms），未发布 ready",
        safe_server_label(.server),
        .timeout_ms
    )]
    Timeout { server: String, timeout_ms: u64 },
    #[error("System MCP 启动已取消")]
    Cancelled,
    #[error("System MCP 启动失败：{source}")]
    RequiredTools {
        #[source]
        source: SystemToolError,
    },
    #[error("System MCP 启动失败：工具目录发布被拒绝，未发布 ready")]
    CatalogPublicationFailed,
}

impl SystemReadinessError {
    /// 映射到 Agent 边界错误（主 plan IF-M3 两条硬约束的落实点，B-03 在
    /// middleware 边界调用）：
    ///
    /// - `Cancelled` → [`AgentError::Interrupted`](peri_agent::error::AgentError::Interrupted)
    ///   （取消不是失败）；
    /// - 其它（**含 timeout**）→
    ///   [`AgentError::MiddlewareError`](peri_agent::error::AgentError::MiddlewareError)，
    ///   reason 取本枚举的固定安全文案（`Middleware error: {middleware} - …`）。
    ///   timeout 不是取消，不得映射 `Interrupted`。
    pub(crate) fn into_agent_error(self, middleware: &str) -> peri_agent::error::AgentError {
        match self {
            SystemReadinessError::Cancelled => peri_agent::error::AgentError::Interrupted,
            other => peri_agent::error::AgentError::MiddlewareError {
                middleware: middleware.to_string(),
                reason: other.to_string(),
            },
        }
    }
}

/// 独立的 readiness 事实与 watch revision。
///
/// 使用独立 watch（**不**复用 UI notifier）：等待方先 `subscribe` 再读事实，任何
/// 变化都会唤醒重读；不持 parking_lot guard 跨 await。
#[derive(Debug)]
pub(crate) struct SystemReadinessTracker {
    manifest: parking_lot::RwLock<SystemMcpManifest>,
    evidence: parking_lot::Mutex<HashMap<String, DiscoveryEvidence>>,
    revision: tokio::sync::watch::Sender<u64>,
}

impl Default for SystemReadinessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemReadinessTracker {
    pub(crate) fn new() -> Self {
        let (revision, _) = tokio::sync::watch::channel(0_u64);
        Self {
            manifest: parking_lot::RwLock::new(SystemMcpManifest::Pending),
            evidence: parking_lot::Mutex::new(HashMap::new()),
            revision,
        }
    }

    /// 订阅变化通知：先订阅再读事实，订阅之后发生的写入不会丢失。
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub(crate) fn manifest(&self) -> SystemMcpManifest {
        *self.manifest.read()
    }

    /// 发布配置清单事实（完整发布 / 加载失败都只唤醒等待方，不产生 ready）。
    pub(crate) fn publish_manifest(&self, manifest: SystemMcpManifest) {
        if *self.manifest.read() == manifest {
            return;
        }
        *self.manifest.write() = manifest;
        self.bump();
    }

    pub(crate) fn evidence(&self, server: &str) -> Option<DiscoveryEvidence> {
        self.evidence.lock().get(server).copied()
    }

    /// 提交本代发现证据（只允许由成功/失败的**生产发现路径**调用）。
    pub(crate) fn commit_evidence(&self, server: &str, evidence: DiscoveryEvidence) {
        self.evidence.lock().insert(server.to_string(), evidence);
        self.bump();
    }

    /// 使某个 server 的证据失效（reconnect / disable / remove / 失败重建）。
    ///
    /// 无论此前是否存在证据都唤醒等待方：新事实必须让等待方立刻重读，而不是
    /// 睡到 deadline。
    pub(crate) fn clear_evidence(&self, server: &str) {
        self.evidence.lock().remove(server);
        self.bump();
    }

    /// 使全部证据失效（pool 关闭等全局事实）。
    pub(crate) fn clear_all_evidence(&self) {
        self.evidence.lock().clear();
        self.bump();
    }

    fn bump(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

/// 当前登记代际；句柄不在表中时返回 `None`（`0` 是「未登记」哨兵，不算身份）。
fn current_generation(pool: &McpClientPool, server: &str) -> Option<u64> {
    pool.get_client(server)
        .map(|handle| pool.handle_generation(&handle))
}

impl McpClientPool {
    /// 发布 System 配置清单事实。**owner：B-02**（`run_initialize`）。
    ///
    /// 契约：完整 `configs`（含空集合、含全部 disabled）一次性写入后发布
    /// `Loaded`；配置加载/校验失败发布 `Failed`。发布 `Loaded` 前 `configs`
    /// 不是可信的 System 依赖事实源。
    pub(crate) fn publish_system_manifest(&self, manifest: SystemMcpManifest) {
        self.system_readiness.publish_manifest(manifest);
    }

    pub(crate) fn system_manifest(&self) -> SystemMcpManifest {
        self.system_readiness.manifest()
    }

    /// 提交本代发现证据。**owner：B-02**（initialize / reconnect / OAuth 成功路径）。
    pub(crate) fn commit_discovery_evidence(&self, server: &str, evidence: DiscoveryEvidence) {
        self.system_readiness.commit_evidence(server, evidence);
    }

    /// 使发现证据失效。**owner：B-02**（重连、重新发现、失败重建前调用）。
    pub(crate) fn clear_discovery_evidence(&self, server: &str) {
        self.system_readiness.clear_evidence(server);
    }

    pub(crate) fn discovery_evidence(&self, server: &str) -> Option<DiscoveryEvidence> {
        self.system_readiness.evidence(server)
    }

    /// 已发布配置清单中的 System 依赖（`system_mcp == Some(true)`），按 server 名排序
    /// 保证遍历确定性（HashMap 顺序不可依赖）。
    ///
    /// 必需工具直接取配置声明（`None` 与 `Some([])` 都是「不注入额外工具」），
    /// 超时取 `system_mcp_timeout` 缺省 [`McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS`]。
    pub(crate) fn system_requirements(&self) -> Vec<SystemMcpRequirement> {
        let mut requirements: Vec<SystemMcpRequirement> = self
            .configs
            .read()
            .iter()
            .filter(|(_, config)| config.system_mcp == Some(true))
            .map(|(server, config)| SystemMcpRequirement {
                server: server.clone(),
                required_tools: config.system_mcp_tools.clone().unwrap_or_default(),
                timeout: system_timeout(config),
            })
            .collect();
        requirements.sort_by(|left, right| left.server.cmp(&right.server));
        requirements
    }

    /// 冻结签名（主 plan IF-M3 / sub-plan B §4.2）：
    ///
    /// ```ignore
    /// pub(crate) async fn await_system_connections(
    ///     self: &Arc<Self>,
    ///     cancel: &peri_agent::agent::AgentCancellationToken,
    ///     started_at: tokio::time::Instant,
    /// ) -> Result<Vec<NegotiatedSystemMcp>, SystemReadinessError>;
    /// ```
    ///
    /// 语义：
    /// - 只等待 `system_mcp == Some(true)` 的 server；普通 MCP（`None` / `false`）
    ///   无论 pending 还是 failed 都不阻塞。
    /// - 清单 `Pending` 时按缺省 30000ms bootstrap 上限等待；`Failed` 立即
    ///   `ConfigurationFailed`，超时为 `ConfigurationUnavailable`。
    /// - 已知失败（Disabled / 需要授权 / transport 失败 / 发现失败 / 无协议证据）
    ///   立即返回；未知/连接中等待到各自的 deadline（从 `started_at` 起算）。
    /// - 已证明完成过的 server 若代际变化，返回 `ConnectionChanged`，不无限重试。
    /// - 取消返回 `Cancelled`（middleware 边界映射 `AgentError::Interrupted`）；
    ///   timeout 不是取消，映射 fatal。
    pub(crate) async fn await_system_connections(
        self: &Arc<Self>,
        cancel: &AgentCancellationToken,
        // 冻结签名：`started_at` 为 `tokio::time::Instant`（本模块 `Instant` 即该类型）。
        started_at: tokio::time::Instant,
    ) -> Result<Vec<NegotiatedSystemMcp>, SystemReadinessError> {
        // 先订阅再读事实：订阅之后的任何变化都不会丢。
        let mut waiter = self.system_readiness.subscribe();
        let mut satisfied: HashMap<String, u64> = HashMap::new();
        loop {
            if cancel.is_cancelled() {
                return Err(SystemReadinessError::Cancelled);
            }
            if !self.is_open() {
                return Err(SystemReadinessError::PoolClosed);
            }
            match self.system_readiness.manifest() {
                SystemMcpManifest::Failed => {
                    return Err(SystemReadinessError::ConfigurationFailed);
                }
                SystemMcpManifest::Pending => {
                    let deadline = started_at + system_bootstrap_timeout();
                    if tokio::time::Instant::now() >= deadline {
                        return Err(SystemReadinessError::ConfigurationUnavailable);
                    }
                    wait_for_readiness_change(&mut waiter, cancel, deadline).await?;
                    continue;
                }
                SystemMcpManifest::Loaded => {}
            }

            let requirements = self.system_requirements();
            if requirements.is_empty() {
                // Loaded 且无 System 依赖：立即通过，不等普通 transport。
                return Ok(Vec::new());
            }

            let now = tokio::time::Instant::now();
            let mut negotiated = Vec::with_capacity(requirements.len());
            let mut next_deadline: Option<tokio::time::Instant> = None;
            for requirement in &requirements {
                let deadline = started_at + requirement.timeout;
                match self.evaluate_system_requirement(requirement)? {
                    Some(found) => {
                        satisfied.insert(found.requirement.server.clone(), found.generation);
                        negotiated.push(found);
                    }
                    None => {
                        // 已证明完成过、随后代际变化：不无限重试，交回调用方重试输入。
                        if let Some(previous) = satisfied.get(&requirement.server) {
                            if current_generation(self, &requirement.server) != Some(*previous) {
                                return Err(SystemReadinessError::ConnectionChanged {
                                    server: requirement.server.clone(),
                                });
                            }
                        }
                        if now >= deadline {
                            return Err(SystemReadinessError::Timeout {
                                server: requirement.server.clone(),
                                timeout_ms: requirement.timeout_ms(),
                            });
                        }
                        next_deadline =
                            Some(next_deadline.map_or(deadline, |current| current.min(deadline)));
                    }
                }
            }
            if negotiated.len() == requirements.len() {
                return Ok(negotiated);
            }
            let deadline = next_deadline.expect("pending requirement must carry its deadline");
            wait_for_readiness_change(&mut waiter, cancel, deadline).await?;
        }
    }

    /// 单台 System server 的 readiness 事实判定。
    ///
    /// `Ok(None)` = 尚未完成（等各自 deadline）；`Ok(Some(_))` = 本代协商完成；
    /// `Err(_)` = 已知失败，立即终止等待。
    fn evaluate_system_requirement(
        &self,
        requirement: &SystemMcpRequirement,
    ) -> Result<Option<NegotiatedSystemMcp>, SystemReadinessError> {
        let server = requirement.server.as_str();
        // 配置层禁用是确定事实，不是「连接中」：不等待、不跳过。
        let configured_disabled = self
            .configs
            .read()
            .get(server)
            .is_some_and(|config| config.disabled.unwrap_or(false));
        if configured_disabled {
            return Err(SystemReadinessError::Disabled {
                server: server.to_string(),
            });
        }
        // 短临界区取出句柄快照后不再持锁，避免与提交路径的锁序交错。
        let Some(handle) = self.get_client(server) else {
            return Ok(None);
        };
        match &handle.status {
            ClientStatus::Disabled => Err(SystemReadinessError::Disabled {
                server: server.to_string(),
            }),
            ClientStatus::Failed(_) if handle.oauth_status == OAuthStatus::NeedsAuthorization => {
                Err(SystemReadinessError::AuthorizationRequired {
                    server: server.to_string(),
                })
            }
            // 失败原因只保留阶段类别：不把 `Failed(String)` 原文写进错误链。
            ClientStatus::Failed(_) => {
                Err(if self.discovery_concluded_with_failure(server, &handle) {
                    SystemReadinessError::ToolDiscoveryFailed {
                        server: server.to_string(),
                    }
                } else {
                    SystemReadinessError::ConnectionFailed {
                        server: server.to_string(),
                    }
                })
            }
            ClientStatus::Disconnected => Err(SystemReadinessError::ConnectionFailed {
                server: server.to_string(),
            }),
            ClientStatus::Uninitialized => Ok(None),
            ClientStatus::Connected => self.evaluate_connected_requirement(requirement, handle),
        }
    }

    /// `Connected` 分支：协议证据 → 本代发现证据 → 完成。
    fn evaluate_connected_requirement(
        &self,
        requirement: &SystemMcpRequirement,
        handle: Arc<McpClientHandle>,
    ) -> Result<Option<NegotiatedSystemMcp>, SystemReadinessError> {
        let server = requirement.server.as_str();
        let generation = self.handle_generation(&handle);
        // 光秃的 Connected（peer=None / 无 peer_info / transport 已关闭 / 未提交
        // service / 未登记代际）既不是协商完成，也不能当「连接中」无限等待。
        if !self.has_protocol_evidence(server, &handle, generation) {
            return Err(SystemReadinessError::NegotiationIncomplete {
                server: server.to_string(),
            });
        }
        match self.discovery_evidence(server) {
            // 无证据 = 发现尝试仍在进行（生产路径在尝试结束时才提交证据）。
            None => Ok(None),
            // 旧代证据：新代重新协商中，旧 Arc 永不被接受。
            Some(evidence) if evidence.generation != generation => Ok(None),
            Some(evidence) if !evidence.initialize_ok => {
                Err(SystemReadinessError::NegotiationIncomplete {
                    server: server.to_string(),
                })
            }
            Some(evidence) if !evidence.tools_list_ok => {
                Err(SystemReadinessError::ToolDiscoveryFailed {
                    server: server.to_string(),
                })
            }
            Some(_) => Ok(Some(NegotiatedSystemMcp {
                requirement: requirement.clone(),
                handle,
                generation,
            })),
        }
    }

    /// 本代协议协商证据：有效 peer + 已协商 peer_info + transport 未关闭 +
    /// 已提交 service + 已登记代际（`0` 是「未登记」哨兵）。
    fn has_protocol_evidence(
        &self,
        server: &str,
        handle: &Arc<McpClientHandle>,
        generation: u64,
    ) -> bool {
        if generation == 0 {
            return false;
        }
        let Some(peer) = handle.peer.as_ref() else {
            return false;
        };
        if peer.peer_info().is_none() || peer.is_transport_closed() {
            return false;
        }
        self.service_committed(server)
    }

    /// service 表内是否存在未进入关闭事务的已提交连接。
    fn service_committed(&self, server: &str) -> bool {
        match self.services.lock().get(server) {
            Some(McpServiceWrapper::Closing(_)) | Some(McpServiceWrapper::Closed) => false,
            Some(_) => true,
            None => false,
        }
    }

    /// 本代证据是否已经把这次发现判定为**失败**（initialize 成功但 `tools/list` 失败）。
    fn discovery_concluded_with_failure(
        &self,
        server: &str,
        handle: &Arc<McpClientHandle>,
    ) -> bool {
        let generation = self.handle_generation(handle);
        self.discovery_evidence(server).is_some_and(|evidence| {
            evidence.generation == generation && evidence.initialize_ok && !evidence.tools_list_ok
        })
    }
}

/// System 启动等待的 bootstrap 上限：清单未知时使用的缺省值。
fn system_bootstrap_timeout() -> Duration {
    Duration::from_millis(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS)
}

fn system_timeout(config: &McpServerConfig) -> Duration {
    // 越界值在配置解析期已被 A 拒绝；此处防御性收敛，避免 `0ms` 退化成即时超时。
    let milliseconds = config
        .system_mcp_timeout
        .unwrap_or(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS)
        .clamp(
            McpServerConfig::MIN_SYSTEM_MCP_TIMEOUT_MS,
            McpServerConfig::MAX_SYSTEM_MCP_TIMEOUT_MS,
        );
    Duration::from_millis(milliseconds)
}

/// 等待 readiness 事实变化、取消或 deadline；返回后由调用方重读全部事实。
///
/// 三路都不持锁、不跨 await 持 guard；`Revision` 与 `Deadline` 的区分对调用方
/// 无意义（重读是幂等的），因此统一返回 `Ok(())`。
async fn wait_for_readiness_change(
    waiter: &mut tokio::sync::watch::Receiver<u64>,
    cancel: &AgentCancellationToken,
    deadline: tokio::time::Instant,
) -> Result<(), SystemReadinessError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(SystemReadinessError::Cancelled),
        // 发送端随 pool 一起销毁：不再有可观察变化，按连接池关闭收口。
        changed = waiter.changed() => changed.map_err(|_| SystemReadinessError::PoolClosed),
        _ = tokio::time::sleep_until(deadline) => Ok(()),
    }
}

#[cfg(test)]
#[path = "readiness_test.rs"]
mod tests;
