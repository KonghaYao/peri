//! session-owned canonical 调用入口（App 回调与 Host 程序化调用的**共同**准入面）。
//!
//! 设计 §3.1 六步管线：归属校验 → 目标绑定 → 既有 before-tool 规则 → 批准覆盖
//! 最终目标 → admission owner 串行裁决（已绑定 peer）→ 先写 outcome 再投影。
//!
//! 本模块的类型隔离（设计 §2）：
//! - [`AppDeliveryToken`] / [`AttachmentDeliveryToken`] / [`InvocationCancel`] 各自
//!   独立，互不转换；`McpAppBindingRegistry` **不暴露** `cancel()`；
//! - [`DispatchCommit`] 不实现 `Clone`，只能被 [`AdmittedPeerTool`] 消费一次；
//! - 因此「关闭 App」与「取消执行」在类型上没有共同 API。
//!
//! 不伪造 turn 事实：idle 执行没有 assistant 消息、没有 `step`、没有 render 事件
//!（设计 §3.2 / §4.1）。

pub(crate) mod admission;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::identity::AgentId;
use peri_acp_types::interaction::{AttachmentId, InteractionAttribution};
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppsErrorKind, McpAppsRelayError, NotExecutedCause,
    SessionInvocationOutcome, SessionInvocationOutcomeRecord, SessionInvocationPort,
    SessionInvocationRequest, SkippedPostHook,
};
use peri_acp_types::mcp_apps_facade::TargetIdentity;
use peri_acp_types::tool_invocation::{EffectiveToolCall, InvocationId, InvocationOrigin};
use peri_acp_types::tools::{
    BaseTool, EffectiveToolError, EffectiveToolErrorCode, ToolContext, ToolOutput,
};
use peri_agent::agent::stages::{
    invocation_dispatcher, ApprovalUnavailableSink, InvocationContext, PostHookSkipSink,
    SessionInvocationSource,
};
use peri_agent::session::factory::InvocationRuleChain;
use peri_agent::session::queue::MessageQueue;
use peri_agent::session::tool_catalog::{CatalogToolEntry, SessionToolCatalogSnapshot, ToolSource};
use peri_agent::tools::DirectToolInvocationResolver;
use tokio_util::sync::CancellationToken;

use self::admission::{PeerBinding, RevocationKey, SessionAdmissionOwner};
use super::apps::{McpAppBindingRegistry, McpAppRecord};
use super::client::{McpClientHandle, McpClientPool};
use super::tool_bridge::McpToolBridge;

/// 会话队列解析器：idle 入口的装配早于 `SessionManager` 登记会话（环境装配 →
/// 会话登记），因此队列必须**按会话现取**，不得在装配期固定一个 detached 队列
/// （那会让 hook 的注入静默丢失）。
pub type SessionQueueLookup = Arc<dyn Fn() -> MessageQueue + Send + Sync>;

/// 结果记录容量（条数）：超出按 FIFO 淘汰（设计 §1 #7 有界要求）。
const MAX_OUTCOME_RECORDS: usize = 256;
/// 单条结果记录的字节预算：超出则**显式不可用**（不截断、不静默丢弃）。
const MAX_OUTCOME_BYTES: usize = 1024 * 1024;

// ─── 五层取消令牌中的三层（App / Attachment / invocation） ─────────────────────

/// App 层交付/撤销令牌（record 持有）。
///
/// 只在 `apps/close` / record 撤销时生效；**不是**执行取消令牌：关闭 App 不得
/// 取消已提交的调用（设计 §2）。
pub struct AppDeliveryToken(CancellationToken);

impl AppDeliveryToken {
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    /// 撤销该 App 的交付资格（幂等）。
    pub fn revoke(&self) {
        self.0.cancel();
    }

    pub fn is_revoked(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Default for AppDeliveryToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Attachment 层交付令牌（连接附件持有）：`detach` / 连接关闭时撤销。
///
/// 同样**不是**执行取消令牌。
pub struct AttachmentDeliveryToken(CancellationToken);

impl AttachmentDeliveryToken {
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    pub fn revoke(&self) {
        self.0.cancel();
    }

    pub fn is_revoked(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Default for AttachmentDeliveryToken {
    fn default() -> Self {
        Self::new()
    }
}

/// 单次调用层取消令牌（service 准入时签发，父为 **session** 层）。
///
/// 请求作用域的令牌只用于**准入前**的放弃判定（见
/// [`SessionInvocationRequest::cancel`]）：已提交的调用不被 App / 连接关闭取消。
pub struct InvocationCancel(CancellationToken);

impl InvocationCancel {
    /// 以 session 令牌为父签发（service 唯一入口）。
    pub(crate) fn issue(session: &CancellationToken) -> Self {
        Self(session.child_token())
    }

    /// 执行期令牌（管线与 interruptible hook 的唯一中断事实源）。
    pub(crate) fn token(&self) -> CancellationToken {
        self.0.clone()
    }
}

// ─── 结果存储（有界） ──────────────────────────────────────────────────────────

/// outcome 存储错误：超限是**显式不可用**，不是静默丢弃。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutcomeStoreError {
    #[error("outcome record exceeds the bounded store budget ({bytes} > {limit} bytes)")]
    PayloadTooLarge { bytes: usize, limit: usize },
}

/// 有界的调用结果记录存储（条数 FIFO 淘汰；单条超预算显式失败）。
#[derive(Default)]
pub struct InvocationOutcomeStore {
    records: Mutex<std::collections::VecDeque<SessionInvocationOutcomeRecord>>,
}

impl InvocationOutcomeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次调用结果；超预算返回 [`OutcomeStoreError::PayloadTooLarge`]。
    pub fn record(&self, record: SessionInvocationOutcomeRecord) -> Result<(), OutcomeStoreError> {
        let bytes = serde_json::to_vec(&record)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if bytes > MAX_OUTCOME_BYTES {
            return Err(OutcomeStoreError::PayloadTooLarge {
                bytes,
                limit: MAX_OUTCOME_BYTES,
            });
        }
        let mut records = self.records.lock();
        records.push_back(record);
        while records.len() > MAX_OUTCOME_RECORDS {
            records.pop_front();
        }
        Ok(())
    }

    /// 已记录条数（有界观测）。
    pub fn len(&self) -> usize {
        self.records.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 最近一条记录（观测 / 测试）。
    pub fn latest(&self) -> Option<SessionInvocationOutcomeRecord> {
        self.records.lock().back().cloned()
    }

    /// 全部记录的副本（观测 / 测试；有界）。
    pub fn snapshot(&self) -> Vec<SessionInvocationOutcomeRecord> {
        self.records.lock().iter().cloned().collect()
    }
}

// ─── 提交后的目标工具 ─────────────────────────────────────────────────────────

/// 提交后的调用目标：先复核提交（撤销 / 换代），再调用**绑定 peer** 的 bridge。
///
/// 管线不改提交语义：审批（含 `Edit` 改参）照常发生在它之前，最终参数原样传给
/// 内层 bridge；复核失败 ⇒ 返回显式错误，**不发** wire 调用。
pub struct AdmittedPeerTool {
    inner: McpToolBridge,
    /// 一次性 wire 资格：第一次投递取走提交，之后不再有可用提交。
    commit: parking_lot::Mutex<Option<admission::DispatchCommit>>,
    bound_effective_name: String,
    /// App record（Host 调用无 record：白名单复核不适用，撤销键落在 session 层）。
    record: Option<Arc<McpAppRecord>>,
    /// 复核失败的精确原因记录点：管线错误类型不足以承载 typed 原因
    /// （`EffectiveToolError` 只有粗粒度 code），service 执行后从本记录点取回
    /// **真实**原因，而不是从错误字符串反推。
    rejection: Arc<parking_lot::Mutex<Option<NotExecutedCause>>>,
}

impl AdmittedPeerTool {
    pub fn new(
        inner: McpToolBridge,
        commit: admission::DispatchCommit,
        record: Option<Arc<McpAppRecord>>,
        rejection: Arc<parking_lot::Mutex<Option<NotExecutedCause>>>,
    ) -> Self {
        let bound_effective_name = inner.name().to_string();
        Self {
            inner,
            commit: parking_lot::Mutex::new(Some(commit)),
            bound_effective_name,
            record,
            rejection,
        }
    }

    /// 最终目标复核（设计 §3.1 第 4 步）：提交有效（未撤销 / 未换代 / 连接在）
    /// 且 effective 名仍在 record 的资格白名单内（`DisplayOnly` 记录没有任何可回调
    /// 工具，因此恒不通过——零 callback 资格）。
    ///
    /// **取走**提交：一次提交只对应一次 wire 资格（重复投递返回 typed
    /// [`NotExecutedCause::CommitConsumed`]，绝不重复发出 wire 调用）。
    fn revalidate(&self) -> Result<(), NotExecutedCause> {
        let result = self.revalidate_inner();
        if let Err(cause) = result {
            *self.rejection.lock() = Some(cause);
        }
        result
    }

    fn revalidate_inner(&self) -> Result<(), NotExecutedCause> {
        let commit = self
            .commit
            .lock()
            .take()
            .ok_or(NotExecutedCause::CommitConsumed)?;
        commit.recheck()?;
        if let Some(record) = self.record.as_ref() {
            if !record.allows_effective_name(&self.bound_effective_name) {
                return Err(NotExecutedCause::TargetUnbound);
            }
        }
        Ok(())
    }
}

/// 提交复核失败时的显式错误（管线把它转成协议层失败，不冒充成功）。
fn not_executed_error(cause: NotExecutedCause) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(EffectiveToolError::new(
        EffectiveToolErrorCode::PermissionDenied,
        format!("MCP App invocation was not executed: {cause:?}"),
    ))
}

#[async_trait]
impl BaseTool for AdmittedPeerTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.inner.mcp_server_name()
    }

    fn mcp_tool_name(&self) -> Option<&str> {
        self.inner.mcp_tool_name()
    }

    fn builtin_mcp_instance(&self) -> Option<&str> {
        self.inner.builtin_mcp_instance()
    }

    fn visible_to_model(&self) -> bool {
        // idle 入口不产生模型可见视图；投递只走 App / Host 面。
        false
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.invoke_output(input, ctx)
            .await
            .map(|output| output.text)
    }

    async fn invoke_output(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        if let Err(cause) = self.revalidate() {
            return Err(not_executed_error(cause));
        }
        self.inner.invoke_output(input, ctx).await
    }
}

// ─── service ──────────────────────────────────────────────────────────────────

/// session-owned canonical 调用入口实现（持有 session 级事实，不持 per-turn 对象）。
pub struct SessionInvocationService {
    pool: Arc<McpClientPool>,
    registry: Arc<McpAppBindingRegistry>,
    session_id: String,
    agent_id: AgentId,
    cwd: Arc<str>,
    queue: SessionQueueLookup,
    rules: Arc<InvocationRuleChain>,
    /// 本会话的 builtin 实例关闭集（与生产 MCP middleware 同一份
    /// `frozen.meta_harness.disabled_middlewares` 投影）：准入视图必须按它过滤，
    /// 否则 Host invoke 可解析到本会话已关闭的实例工具（绕过 MetaHarness）。
    closed_instances: BTreeSet<String>,
    admission: Arc<SessionAdmissionOwner>,
    outcomes: InvocationOutcomeStore,
    /// session 层取消令牌（invocation 令牌的父；会话关闭即中断执行）。
    session_cancel: CancellationToken,
}

/// session 级构造输入（全部为 session 级事实；不含 turn 输入）。
pub struct SessionInvocationInputs {
    pub pool: Arc<McpClientPool>,
    /// 本会话的 agent 身份（事件纽带；idle 执行不发事件）。
    pub agent_id: AgentId,
    pub session_id: String,
    pub cwd: Arc<str>,
    /// 会话队列现取入口（hook 输出端口；投影语义与 `build_stage_context` 一致）。
    pub queue: SessionQueueLookup,
    /// idle 准入规则链（[`InvocationRuleChain`]：`Hook` + `Permission` 派生窄链；
    /// 与 per-turn 生产链**不同类型**，装配点不得注入轮次链）。
    pub rules: Arc<InvocationRuleChain>,
    /// 本会话 builtin 实例关闭集（`frozen.meta_harness.disabled_middlewares`
    /// 的 `closed_instances` 投影；与 `McpMiddleware` 同一份事实源）。
    pub closed_instances: BTreeSet<String>,
    /// session 层取消令牌。
    pub session_cancel: CancellationToken,
}

impl SessionInvocationService {
    /// 构造（record 注册表取 deployment pool 上的同一实例）。
    pub fn new(inputs: SessionInvocationInputs) -> Self {
        let registry = Arc::clone(&inputs.pool.app_binding_leases);
        Self {
            registry,
            admission: Arc::new(SessionAdmissionOwner::new(1)),
            outcomes: InvocationOutcomeStore::new(),
            pool: inputs.pool,
            session_id: inputs.session_id,
            agent_id: inputs.agent_id,
            cwd: inputs.cwd,
            queue: inputs.queue,
            rules: inputs.rules,
            closed_instances: inputs.closed_instances,
            session_cancel: inputs.session_cancel,
        }
    }

    /// runtime epoch（装配期签发；请求必须携带同一值）。
    pub fn runtime_epoch(&self) -> u64 {
        self.admission.runtime_epoch()
    }

    /// 结果记录（观测 / 测试；有界）。
    pub fn outcomes(&self) -> &InvocationOutcomeStore {
        &self.outcomes
    }

    /// 会话层取消令牌是否已被取消（观测：会话关闭语义）。
    pub fn session_cancelled(&self) -> bool {
        self.session_cancel.is_cancelled()
    }

    /// 记录一次 outcome（超预算显式告警，不静默丢弃）。
    fn store(&self, record: SessionInvocationOutcomeRecord) {
        if let Err(error) = self.outcomes.record(record) {
            tracing::warn!(
                session_id = %self.session_id,
                %error,
                "MCP App invocation outcome was not retained"
            );
        }
    }

    /// 绑定 App record 目标：身份 / 附件 / 白名单 / 世代全部复核。
    fn bind_app_target(
        &self,
        record_id: &str,
        tool_name: &str,
        origin: &InvocationOrigin,
    ) -> Result<BoundTarget, NotExecutedCause> {
        let InvocationOrigin::App {
            app_id,
            attachment_id,
            connection_epoch,
            server_generation,
        } = origin
        else {
            return Err(NotExecutedCause::UntrustedOwner);
        };
        if app_id != record_id {
            // 来源自述与目标身份不一致：不铸造归属，也不执行。
            return Err(NotExecutedCause::UntrustedOwner);
        }
        let record = self
            .registry
            .find(record_id)
            .ok_or(NotExecutedCause::TargetRevoked)?;
        if record.owner_session_id != self.session_id {
            return Err(NotExecutedCause::UntrustedOwner);
        }
        if !record.is_attached() {
            return Err(NotExecutedCause::TargetRevoked);
        }
        // 调用方自述的附件事实必须与**该附件**的记录一致：按附件身份在记录的
        // 附件**集合**中定位（多附件 / 多 View 下，一个附件 detach 不影响其他
        // 附件与 record 本体），再复核该附件的纪元。缺任一事实 = 无归属。
        let (Some(attachment_id), Some(connection_epoch)) =
            (attachment_id.as_ref(), connection_epoch.as_ref())
        else {
            return Err(NotExecutedCause::UntrustedOwner);
        };
        let route = record
            .attachment_for(attachment_id)
            .ok_or(NotExecutedCause::TargetRevoked)?;
        if &route.connection_epoch != connection_epoch {
            return Err(NotExecutedCause::UntrustedOwner);
        }
        if *server_generation != record.server_generation {
            return Err(NotExecutedCause::TargetRevoked);
        }
        let effective = record
            .allowed_effective_name(tool_name)
            .cloned()
            .ok_or(NotExecutedCause::TargetUnbound)?;
        let (bridge, peer) = self.resolve_bridge(&record.server_id, &effective)?;
        if self.pool.handle_generation(&peer) != record.server_generation || peer.peer.is_none() {
            return Err(NotExecutedCause::TargetRevoked);
        }
        let target = target_identity(&record.server_id, bridge.mcp_tool_name(), &effective);
        Ok(BoundTarget {
            key: RevocationKey::App(record.record_id.clone()),
            server_generation: record.server_generation,
            record: Some(record),
            effective_name: effective,
            target,
            bridge,
            peer,
        })
    }

    /// 绑定 Host 目标：server + wire 工具名必须在会话允许视图内。
    fn bind_host_target(
        &self,
        server_id: &str,
        tool_name: &str,
        origin: &InvocationOrigin,
    ) -> Result<BoundTarget, NotExecutedCause> {
        if !matches!(origin, InvocationOrigin::Host { .. }) {
            return Err(NotExecutedCause::UntrustedOwner);
        }
        let bridge = self
            .session_visible_bridges()
            .into_iter()
            .find(|bridge| {
                bridge.mcp_server_name() == Some(server_id)
                    && bridge.mcp_tool_name() == Some(tool_name)
            })
            .ok_or(NotExecutedCause::TargetUnbound)?;
        let peer = Arc::clone(bridge.client_handle());
        let server_generation = self.pool.handle_generation(&peer);
        if peer.peer.is_none() {
            return Err(NotExecutedCause::TargetRevoked);
        }
        let effective_name = bridge.name().to_string();
        let target = target_identity(server_id, bridge.mcp_tool_name(), &effective_name);
        Ok(BoundTarget {
            // Host 调用没有 App record：撤销键落在会话层（会话关闭即未执行）。
            key: RevocationKey::Session,
            record: None,
            effective_name,
            target,
            bridge,
            peer,
            server_generation,
        })
    }

    /// 会话允许视图：与 `McpMiddleware` 同一准入原语（不重枚举 pool），且
    /// **携带本会话的关闭集**——关闭的 builtin 实例工具不在准入视图内。
    fn session_visible_bridges(&self) -> Vec<McpToolBridge> {
        super::admission::McpAdmissionPreparation::new(
            Arc::clone(&self.pool),
            Some(self.session_id.clone()),
            self.session_cancel.clone(),
            self.closed_instances.clone(),
        )
        .open_typed_bridges()
    }

    /// 在会话允许视图内解析 effective 名（同名冒充在此被拒绝）。
    fn resolve_bridge(
        &self,
        server_id: &str,
        effective_name: &str,
    ) -> Result<(McpToolBridge, Arc<McpClientHandle>), NotExecutedCause> {
        let bridge = self
            .session_visible_bridges()
            .into_iter()
            .find(|bridge| {
                bridge.name() == effective_name && bridge.mcp_server_name() == Some(server_id)
            })
            .ok_or(NotExecutedCause::TargetUnbound)?;
        let peer = Arc::clone(bridge.client_handle());
        Ok((bridge, peer))
    }

    async fn run(
        &self,
        request: &SessionInvocationRequest,
        invocation_id: &InvocationId,
    ) -> (
        SessionInvocationOutcome,
        Vec<SkippedPostHook>,
        Option<TargetIdentity>,
    ) {
        // 1. 归属与身份（fail-closed；全部零 wire 调用）。
        let rejected = |cause: NotExecutedCause| {
            (
                SessionInvocationOutcome::NotExecuted { cause },
                Vec::new(),
                None,
            )
        };
        if request.session_id != self.session_id {
            return rejected(NotExecutedCause::UntrustedOwner);
        }
        if InteractionAttribution::from_invocation_origin(&request.origin).is_none() {
            // 没有可信归属（例如未附着的 App 来源）：交互无法路由 ⇒ 未执行。
            return rejected(NotExecutedCause::UntrustedOwner);
        }
        if request.runtime_epoch != self.runtime_epoch() {
            return rejected(NotExecutedCause::TargetRevoked);
        }
        if request.cancel.is_cancelled() {
            return rejected(NotExecutedCause::CancelledBeforeCommit);
        }

        // 2. 目标绑定（App record / Host server+tool）。
        let (bound, arguments) = match &request.target {
            InvocationTarget::App {
                record_id,
                tool_name,
                arguments,
            } => (
                match self.bind_app_target(record_id, tool_name, &request.origin) {
                    Ok(bound) => bound,
                    Err(cause) => return rejected(cause),
                },
                arguments.clone(),
            ),
            InvocationTarget::Host {
                server_id,
                tool_name,
                arguments,
            } => (
                match self.bind_host_target(server_id, tool_name, &request.origin) {
                    Ok(bound) => bound,
                    Err(cause) => return rejected(cause),
                },
                arguments.clone(),
            ),
        };

        // 3./5. admission owner 串行裁决：提交前撤销胜出 ⇒ 上游零调用。
        let commit = match self.admission.commit(
            bound.key.clone(),
            request.runtime_epoch,
            PeerBinding {
                pool: Arc::clone(&self.pool),
                peer: Arc::clone(&bound.peer),
                server_generation: bound.server_generation,
                record: bound.record.clone(),
            },
        ) {
            Ok(commit) => commit,
            Err(cause) => return rejected(cause),
        };

        // 4. 既有 before-tool 规则（Hook PreToolUse + Permission）与执行：同一条
        //    canonical 管线；目标被包成已提交工具，wire 调用前复核提交。
        let rejection: Arc<parking_lot::Mutex<Option<NotExecutedCause>>> = Arc::default();
        let admitted: Arc<dyn BaseTool> = Arc::new(AdmittedPeerTool::new(
            bound.bridge,
            commit,
            bound.record.clone(),
            Arc::clone(&rejection),
        ));
        // 目标身份先取副本：bound 的其余部分被移入已提交工具。
        let target = bound.target.clone();
        let catalog = Arc::new(single_tool_catalog(&bound.effective_name, admitted));

        let skips = Arc::new(PostHookSkipSink::new());
        let unanswered = Arc::new(ApprovalUnavailableSink::new());
        let invocation_cancel = InvocationCancel::issue(&self.session_cancel);
        let ctx = InvocationContext::from_source(SessionInvocationSource {
            session_id: Some(self.session_id.clone()),
            run_id: None,
            agent_id: self.agent_id,
            cwd: Arc::clone(&self.cwd),
            messages: Arc::new(Vec::new()),
            context_usage: None,
            session_resources: None,
            resolver: Arc::new(DirectToolInvocationResolver),
            chain: self.rules.shared(),
            queue: (self.queue)(),
            origin: request.origin.clone(),
            cancel: invocation_cancel.token(),
            post_hook_skips: Some(Arc::clone(&skips)),
            approval_unavailable: Some(Arc::clone(&unanswered)),
        });
        let dispatcher = invocation_dispatcher(ctx, catalog);
        let result = dispatcher
            .dispatch(
                EffectiveToolCall {
                    invocation_id: invocation_id.clone(),
                    tool_name: bound.effective_name.clone(),
                    input: serde_json::Value::Object(arguments),
                    origin: request.origin.clone(),
                },
                invocation_cancel.token(),
            )
            .await;

        let outcome = match result {
            Ok(output) => match output.raw() {
                Some(raw) => SessionInvocationOutcome::Completed(raw.clone()),
                None => SessionInvocationOutcome::OutcomeUnknown {
                    reason: format!(
                        "MCP server returned no raw result payload for `{}`",
                        bound.effective_name
                    ),
                },
            },
            Err(error) => {
                // 提交后复核（撤销 / 换代 / 一次性资格）失败的**真实**原因优先：
                // 管线的粗粒度 code 只作兜底（裁决：策略拒绝 ≠ 目标撤销）。
                match rejection.lock().take() {
                    Some(cause) => SessionInvocationOutcome::NotExecuted { cause },
                    None => map_dispatch_error(
                        error,
                        unanswered.take(),
                        self.session_cancel.is_cancelled(),
                    ),
                }
            }
        };
        (outcome, skips.take(), Some(target))
    }

    /// 记录一次调用结果（含显式跳过的事后钩子，裁决 R2）。
    fn finish(
        &self,
        invocation_id: InvocationId,
        target: Option<TargetIdentity>,
        outcome: SessionInvocationOutcome,
        skipped_post_hooks: Vec<SkippedPostHook>,
    ) -> SessionInvocationOutcome {
        self.store(SessionInvocationOutcomeRecord {
            invocation_id,
            target,
            outcome: outcome.clone(),
            skipped_post_hooks,
        });
        outcome
    }
}

/// 已绑定的调用目标（App / Host 共用形状）。
struct BoundTarget {
    /// 撤销键：App record → `App`，Host 调用 → `Session`。
    key: RevocationKey,
    /// App record（Host 调用为 `None`）。
    record: Option<Arc<McpAppRecord>>,
    effective_name: String,
    /// 服务端导出的目标身份（rerun 的唯一权威来源，裁决 D4）。
    target: TargetIdentity,
    bridge: McpToolBridge,
    peer: Arc<McpClientHandle>,
    server_generation: u64,
}

/// 从**服务端事实**构造目标身份（客户端字段不参与）。
///
/// `wire_tool_name` 取 MCP server 声明的原始名：`McpToolBridge` 由池内目录构造，
/// 该字段恒存在；万一缺失则留空串，绝不拿 effective 名或客户端字段顶替
/// （空名在 rerun 绑定期会被显式拒绝，属 fail-closed）。
fn target_identity(
    server_id: &str,
    wire_tool_name: Option<&str>,
    effective_tool_name: &str,
) -> TargetIdentity {
    TargetIdentity {
        server_id: server_id.to_string(),
        wire_tool_name: wire_tool_name.unwrap_or_default().to_string(),
        effective_tool_name: effective_tool_name.to_string(),
    }
}

/// 单目标视图：只含本次已绑定目标，避免同名冒充与解析歧义。
fn single_tool_catalog(name: &str, tool: Arc<dyn BaseTool>) -> SessionToolCatalogSnapshot {
    let entry = CatalogToolEntry {
        tool,
        source: ToolSource::CoreOrMiddleware,
    };
    SessionToolCatalogSnapshot {
        generation: 0,
        tools: BTreeMap::from([(name.to_string(), entry)]),
        direct_definitions: Vec::new(),
        aliases: BTreeMap::new(),
    }
}

/// 分发失败 → typed 结果事实（不把「未执行」伪装成执行失败）。
fn map_dispatch_error(
    error: EffectiveToolError,
    unanswered: Option<peri_acp_types::interaction::UnansweredCause>,
    session_cancelled: bool,
) -> SessionInvocationOutcome {
    if let Some(cause) = unanswered {
        // 裁决 R9：无人可答 ≠ 用户拒绝（两者在 outcome 记录层必须可区分）。
        return SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(cause),
        };
    }
    match error.code {
        EffectiveToolErrorCode::UserRejected => SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UserRejected,
        },
        // 策略层拒绝（如审批中间件在无审批模式下拒绝）：目标仍有效、也没有用户
        // 决定发生，不得记成「目标已撤销」。
        EffectiveToolErrorCode::PermissionDenied => SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::PolicyDenied,
        },
        EffectiveToolErrorCode::UnknownTool => SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::TargetUnbound,
        },
        EffectiveToolErrorCode::Cancelled if !session_cancelled => {
            SessionInvocationOutcome::NotExecuted {
                cause: NotExecutedCause::CancelledBeforeCommit,
            }
        }
        _ => match error.remote_error.and_then(|remote| remote.jsonrpc) {
            Some(jsonrpc) => SessionInvocationOutcome::RemoteProtocolError(jsonrpc),
            None => SessionInvocationOutcome::OutcomeUnknown {
                reason: error.message,
            },
        },
    }
}

#[async_trait]
impl SessionInvocationPort for SessionInvocationService {
    async fn invoke(
        &self,
        request: SessionInvocationRequest,
    ) -> Result<SessionInvocationOutcome, McpAppsRelayError> {
        if request.session_id != self.session_id {
            // 会话不匹配不是一次调用：不产生执行身份，也不写 outcome。
            return Err(McpAppsRelayError {
                kind: McpAppsErrorKind::InvalidSession,
            });
        }
        let invocation_id = request
            .invocation_id
            .clone()
            .unwrap_or_else(InvocationId::generate);
        let (outcome, skips, target) = self.run(&request, &invocation_id).await;
        Ok(self.finish(invocation_id, target, outcome, skips))
    }

    fn source_target(&self, session_id: &str, source: &InvocationId) -> Option<TargetIdentity> {
        // 会话不符不是「查不到记录」：本服务只保留本会话的调用结果，跨会话一律
        // 显式不可导出（ID 只定位，不授权）。
        if session_id != self.session_id {
            return None;
        }
        self.outcomes
            .snapshot()
            .into_iter()
            .rev()
            .find(|record| &record.invocation_id == source)
            .and_then(|record| record.target)
    }

    fn session_closing(&self) {
        self.session_cancel.cancel();
        let attachment_ids = self.registry.revoke_session(&self.session_id);
        self.admission.revoke(RevocationKey::Session);
        for attachment_id in attachment_ids {
            self.admission
                .revoke(RevocationKey::Attachment(attachment_id));
        }
    }

    fn revoke_app(&self, app_id: &str) {
        self.registry.revoke_app(app_id);
        self.admission
            .revoke(RevocationKey::App(app_id.to_string()));
    }

    fn revoke_attachment(&self, attachment_id: &AttachmentId) {
        // 附件级撤销**只**作用于该附件（裁决 D6）：按附件身份定位持有它的记录，
        // 只摘掉这一个附件；record 本体与其他附件都不受影响（连接关闭不再是
        // record 的终态撤销）。附件身份与 record 身份是两类身份，不得互相代替。
        if let Some(record) = self.registry.record_for_attachment(attachment_id) {
            record.detach_attachment(attachment_id);
        }
        // 附件级准入键：撤掉「该附件名下未提交调用」的准入资格。注意 App 调用的
        // 提交键是 `App(record_id)`（见 `bind_app_target`），因此本行只对未来
        // 以附件为键的准入面生效；已提交调用不受影响（撤销不取消执行）。
        self.admission
            .revoke(RevocationKey::Attachment(attachment_id.clone()));
    }
}

#[cfg(test)]
#[path = "session_invocation_test.rs"]
mod tests;
