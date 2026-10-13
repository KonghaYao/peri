//! canonical 工具执行的会话侧上下文。
//!
//! 一次 canonical 执行（Act 批次的单条调用、嵌套调用、App / Host 调用）从这里读取
//! 会话事实与输出端口；**不持有 [`StageContext`]**。App / Host 租约会长期存活，
//! 持有阶段上下文会把整个 session（transcript / queue / tools / LLM / middleware
//! chain）挂在租约上，并让 `registry → lease.dispatcher → catalog →
//! McpToolBridge.binding_leases → registry` 形成强引用环。
//!
//! 本模块只投影事实与端口：
//! - 事实（只读）：session 身份、cwd、可见消息快照、上下文用量、会话资源、执行来源、turn 事实；
//! - 端口（受控写）：会话队列（[`InvocationQueuePort`]）、render 事件（[`EventBus::emit_render`]）；
//! - 中断事实源：执行入口的取消令牌（[`InvocationContext::cancellation`]）。
//!
//! 不提供伪造 assistant 消息、`step = 0` 或可写 no-op 状态的能力：需要真实 turn 的
//! 入口用 [`InvocationContext::require_turn`] 显式返回
//! [`AgentError::NotAvailableWithoutTurn`]，而不是静默降级或伪造 turn 事实。
//!
//! idle（无活跃 turn）执行入口由 [`InvocationContext::from_source`] 构造：
//! 触发方提供 session 级事实与 typed 来源，`turn: None` 是**事实缺失**而不是被
//! 清空。需要 turn 的规则走 [`InvocationContext::require_model_turn`]（模型来源
//! 返回 typed [`AgentError::NotAvailableWithoutTurn`]）；事后钩子在无 turn 时
//! 显式记录跳过（裁决 R2），不得回退到伪造事实。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use peri_acp_types::interaction::UnansweredCause;
use peri_acp_types::mcp_apps::SkippedPostHook;

use super::StageContext;
use crate::agent::events_v2::{EventBus, RenderEvent};
use crate::error::{AgentError, AgentResult};
use crate::messages::BaseMessage;
use crate::middleware::MiddlewareChain;
use crate::session::turn::TurnId;
use crate::session::{MessageQueue, QueuedMessage};
use crate::tools::{ContextUsage, InvocationOrigin, ToolCallId, ToolInvocationResolver};
use peri_acp_types::identity::AgentId;
use peri_acp_types::interaction::InteractionAttribution;
use peri_acp_types::session::ExecutionBinding;
use peri_acp_types::session_resources::SessionResources;

/// 当前 turn 的只读事实。
///
/// 只记录 turn 身份与步进，不持有 turn 的 cancel token：turn 令牌与
/// `SessionConfig` 共享，是 session 级取消而不是 turn 事实；执行级中断的唯一
/// 事实源是 [`InvocationContext::cancellation`]（执行入口令牌）。
#[derive(Debug, Clone)]
pub struct ModelTurnFacts {
    /// turn / attempt 身份（事件纽带与 middleware `execution_binding` 投影）。
    binding: ExecutionBinding,
    /// 当前 ReAct step（turn 内循环迭代次数）。
    step: usize,
}

impl ModelTurnFacts {
    /// 从阶段上下文投影当前 turn 事实。
    pub(crate) fn from_stage(stage: &StageContext) -> Self {
        Self {
            binding: stage.session.turn.execution_binding(),
            step: stage.session.turn.current_step(),
        }
    }

    pub(crate) fn turn_id(&self) -> TurnId {
        self.binding.turn_id
    }

    pub(crate) fn execution_binding(&self) -> ExecutionBinding {
        self.binding.clone()
    }

    pub(crate) fn step(&self) -> usize {
        self.step
    }
}

/// canonical 执行的入口来源。
///
/// 模型批次入口一次 dispatch 覆盖多条调用，每条执行有各自的模型 tool-call ID；
/// App / Host / 嵌套入口是单条执行，来源整批一致。逐条执行的来源由
/// [`ExecutionEntryOrigin::for_call`] 特化，调用方不得用字符串前缀推断来源。
#[derive(Clone)]
pub enum ExecutionEntryOrigin {
    /// 模型 assistant 消息发起的工具调用批次。
    ModelBatch,
    /// 单条执行入口（App / Host / 嵌套 / 显式模型调用）。
    Single(InvocationOrigin),
}

impl ExecutionEntryOrigin {
    /// 单条执行的来源：模型批次用该条调用的 tool-call ID 特化。
    pub(crate) fn for_call(&self, model_tool_call_id: &ToolCallId) -> InvocationOrigin {
        match self {
            Self::ModelBatch => InvocationOrigin::Model {
                tool_call_id: model_tool_call_id.clone(),
            },
            Self::Single(origin) => origin.clone(),
        }
    }
}

/// 执行期会话队列的受控写入端口。
///
/// 只提供 canonical 投递与会话队列引用（供 middleware `QueueState` 窄接口复用）；
/// 不暴露 drain、替换或清空能力。唤醒语义由消息自身策略决定。
#[derive(Clone)]
pub(crate) struct InvocationQueuePort {
    queue: MessageQueue,
}

impl InvocationQueuePort {
    /// 投递一条 canonical 队列消息。
    pub(crate) fn enqueue(&self, message: QueuedMessage) {
        self.queue.push(message);
    }

    /// 会话队列引用（`QueueState::v2_queue` 的事实源）。
    pub(crate) fn queue(&self) -> &MessageQueue {
        &self.queue
    }
}

/// 一次 canonical 执行的会话侧事实与端口。
///
/// 由执行入口（Act 批次 / 嵌套调用 / 后续的 App / Host 入口）投影一次，之后按
/// 单条调用派生（[`Self::origin_for`]）。所有字段都是快照或端口，克隆廉价且不
/// 反向持有 session。
#[derive(Clone)]
pub struct InvocationContext {
    /// canonical session 身份（工具读取的 `ToolContext.session_id`）；`None` = 本次
    /// 执行没有真实 ACP 会话身份（如子 agent 自建 session），交互归属因此不可铸造。
    session_id: Option<String>,
    /// ReAct run 关联键（metrics / tracing）；未提供时为 `None`。
    run_id: Option<String>,
    /// 本 session 的 agent 身份（render 事件纽带）。
    agent_id: AgentId,
    /// dispatch 时刻的工作目录。
    cwd: Arc<str>,
    /// dispatch 时刻的可见消息快照（**不含**本批 AI 消息：提交前视图）。
    messages: Arc<Vec<BaseMessage>>,
    /// dispatch 时刻的上下文用量；无预算或无估算时为 `None`。
    context_usage: Option<ContextUsage>,
    /// 会话资源端口（idempotent reminder 等）。
    session_resources: Option<Arc<dyn SessionResources>>,
    /// canonical 目标解析端口：嵌套 / App 调用用 pinned catalog 走同一解析路径。
    resolver: Arc<dyn ToolInvocationResolver>,
    /// middleware chain 端口：canonical 执行独占 before_tools_batch /
    /// after_tool / on_error 三个阶段的钩子，审批与权限规则与 Act 同一链。
    chain: Arc<MiddlewareChain>,
    /// 执行入口来源。
    entry_origin: ExecutionEntryOrigin,
    /// 当前 turn 事实；`None` = 无活跃 turn。
    turn: Option<ModelTurnFacts>,
    /// 执行入口的**环境归属事实**：turn 来源为 [`InteractionAttribution::ModelTurn`]
    /// （真实会话身份 + turn 绑定），idle 入口为 App / Host 归属。
    ///
    /// `None` = 本次执行没有可信归属（例如没有真实会话身份）：需要交互的规则
    /// 据此显式拒绝，不从 agent_id / 字符串反推。
    attribution: Option<InteractionAttribution>,
    /// 会话队列受控端口。
    queue: InvocationQueuePort,
    /// render 事件通知端口。
    ///
    /// `None` = 本次执行**按构造**没有模型可见事件端口（App / Host idle 入口）：
    /// 不产生 `ToolStarted/ToolEnded` 卡片，不冒充模型工具调用（设计 §4.1）。
    events: Option<Arc<EventBus>>,
    /// 执行入口的取消令牌（唯一中断事实源）。
    cancellation: CancellationToken,
    /// 无 turn 时被显式跳过的事后钩子的记录点（裁决 R2）；`None` = 调用方不要求
    /// 记录（turn 来源的执行不会跳过）。
    post_hook_skips: Option<Arc<PostHookSkipSink>>,
    /// 「审批不可用（无人可答）」的记录点（裁决 R9）；`None` = 调用方不消费
    ///（轮次执行由 `AgentError` 层承载）。
    approval_unavailable: Option<Arc<ApprovalUnavailableSink>>,
}

/// 「审批不可用（无人可答）」事实的有界记录点（设计 §3.4 / 裁决 R9）。
///
/// 与「用户拒绝」严格区分：拒绝是**已作出的决定**，不可用是**没有人可以决定**。
/// 调用期窄状态把它记进本次执行的 outcome 元数据；`AgentError` 层的 typed 分类
/// 留待阶段 4（`AgentError::NotAvailableWithoutTurn` 之外的审批细分仍共享
/// `ToolRejected`）。
#[derive(Default)]
pub struct ApprovalUnavailableSink {
    cause: parking_lot::Mutex<Option<UnansweredCause>>,
}

impl ApprovalUnavailableSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次不可用原因（首次胜出：一次执行至多一个准入决定）。
    pub fn record(&self, cause: UnansweredCause) {
        let mut slot = self.cause.lock();
        if slot.is_none() {
            *slot = Some(cause);
        }
    }

    /// 取走已记录的原因（消费一次）。
    pub fn take(&self) -> Option<UnansweredCause> {
        self.cause.lock().take()
    }
}

/// 事后钩子跳过事实的有界记录点（裁决 R2）。
///
/// 只有两个变体（`after_tool` / `on_error`），因此容量天然有界：重复记录去重。
/// 调用方（idle 执行入口）在 dispatch 之后取走事实并写进 outcome 记录。
#[derive(Default)]
pub struct PostHookSkipSink {
    skips: parking_lot::Mutex<Vec<SkippedPostHook>>,
}

impl PostHookSkipSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次跳过（幂等：同一钩子只留一条）。
    pub fn record(&self, hook: SkippedPostHook) {
        let mut skips = self.skips.lock();
        if !skips.contains(&hook) {
            skips.push(hook);
        }
    }

    /// 取走已记录的跳过事实（消费一次，避免跨调用串味）。
    pub fn take(&self) -> Vec<SkippedPostHook> {
        std::mem::take(&mut *self.skips.lock())
    }
}

impl InvocationContext {
    /// 从阶段上下文投影一次执行入口的事实与端口。
    ///
    /// `cancellation` 是本次执行入口的取消令牌（Act 批次 = turn 令牌；嵌套 / App
    /// 调用 = `EffectiveToolDispatcher::dispatch` 收到的令牌），执行管线与
    /// interruptible hook 都以它作为中断事实源。
    pub(crate) fn from_stage(
        stage: &StageContext,
        entry_origin: ExecutionEntryOrigin,
        cancellation: CancellationToken,
    ) -> Self {
        // metrics / tracing 键值对的显式投影：键值表本身不进调用期事实，
        // 只保留调用期真正读取的两项（session 身份与 run 关联键）。
        // 投影规则单一权威：`StageContext::project_session_id`（先取值再取锁，
        // 避免在持有读锁时再次进入同一锁）；缺失即 `None`，不用 agent_id 伪造。
        let session_id = stage.project_session_id();
        let run_id = stage.session.session_context.read().get("run_id").cloned();
        let context_usage = stage.compact.context_budget.as_ref().and_then(|budget| {
            stage
                .compact
                .token_tracker
                .read()
                .estimated_context_tokens()
                .map(|used_tokens| ContextUsage {
                    used_tokens,
                    context_window: budget.context_window,
                })
        });
        let session_resources = stage
            .session
            .transcript
            .read()
            .idempotent_reminder_port()
            .map(|(resources, _, _)| resources);
        let turn = ModelTurnFacts::from_stage(stage);
        // 归属事实与会话身份同源：会话身份缺失即 `None`（不伪造），turn 绑定取真实事实。
        let attribution = session_id.clone().map(|session_id| {
            InteractionAttribution::model_turn(session_id, turn.execution_binding())
        });
        Self {
            session_id,
            run_id,
            agent_id: stage.session.agent_id,
            cwd: Arc::clone(&stage.session.turn.cwd),
            messages: Arc::new(stage.visible_messages()),
            context_usage,
            session_resources,
            resolver: Arc::clone(&stage.runtime.tool_invocation_resolver),
            chain: Arc::clone(&stage.runtime.middleware_chain),
            entry_origin,
            attribution,
            turn: Some(turn),
            queue: InvocationQueuePort {
                queue: stage.session.queue.clone(),
            },
            events: Some(Arc::clone(&stage.runtime.event_bus)),
            cancellation,
            // 轮次入口不跳过事后钩子（有 turn），因此不设跳过记录点。
            post_hook_skips: None,
            approval_unavailable: None,
        }
    }

    /// 同一执行入口下的单条执行上下文：来源整批一致（App / Host / 嵌套）。
    pub(crate) fn with_single_origin(&self, origin: InvocationOrigin) -> Self {
        let mut context = self.clone();
        context.entry_origin = ExecutionEntryOrigin::Single(origin);
        context
    }

    /// 单条执行的取消令牌：以本次执行入口收到的令牌为准。
    ///
    /// 嵌套 / App 执行的令牌通常是外层执行令牌的子令牌（撤销租约同样中断本次
    /// 执行的 hook），因此不会丢失外层取消的传递。
    pub(crate) fn with_cancellation(&self, cancellation: CancellationToken) -> Self {
        let mut context = self.clone();
        context.cancellation = cancellation;
        context
    }

    /// 单条执行的来源；模型批次用该条调用自己的 tool-call ID 特化。
    pub(crate) fn origin_for(&self, model_tool_call_id: &ToolCallId) -> InvocationOrigin {
        self.entry_origin.for_call(model_tool_call_id)
    }

    /// 当前 turn 事实；无活跃 turn 时为 `None`。
    pub(crate) fn turn(&self) -> Option<&ModelTurnFacts> {
        self.turn.as_ref()
    }

    /// 本次执行的环境归属事实（turn 来源 = 真实会话身份 + turn 绑定；idle 入口 =
    /// App / Host 归属）；无可信归属时为 `None`。
    pub(crate) fn attribution(&self) -> Option<&InteractionAttribution> {
        self.attribution.as_ref()
    }

    /// 单条执行的归属事实。
    ///
    /// - App / Host 来源：从 typed 来源**派生**（谁发起就是谁在问），未附着附件
    ///   的 App 来源返回 `None`（无可路由 endpoint，fail-closed）；
    /// - 模型 / 嵌套来源：沿用环境归属（真实 turn 事实），不从 tool-call ID 反推。
    pub(crate) fn attribution_for(
        &self,
        origin: &InvocationOrigin,
    ) -> Option<InteractionAttribution> {
        match origin {
            InvocationOrigin::App { .. } | InvocationOrigin::Host { .. } => {
                InteractionAttribution::from_invocation_origin(origin)
            }
            InvocationOrigin::Model { .. } | InvocationOrigin::Nested { .. } => {
                self.attribution.clone()
            }
        }
    }

    /// 取真实 turn 事实；无 turn 时显式拒绝。
    ///
    /// turn 是准入事实而不是可选装饰：需要 turn 的入口返回 typed
    /// [`AgentError::NotAvailableWithoutTurn`]（带入口名与已知的 session 身份），
    /// 而不是伪造 `step = 0`、静默跳过或退化成泛化 `Other` 错误。
    pub(crate) fn require_turn(&self, entry: &'static str) -> AgentResult<&ModelTurnFacts> {
        self.turn
            .as_ref()
            .ok_or(AgentError::NotAvailableWithoutTurn {
                entry,
                session_id: self.session_id.clone(),
            })
    }

    /// **模型来源**执行的 turn 事实：无 turn 时返回 typed 不可用。
    ///
    /// 模型来源（模型批次 / 显式模型调用）按定义发生在模型 turn 内：turn 缺失
    /// 说明本次执行被要求提供 turn 绑定的事实（render 事件纽带、step、
    /// `turn_generation`）而没有，因此必须**显式不可用**而不是静默降级。
    /// App / Host / 嵌套来源没有 turn 绑定义务，返回 `Ok(None)`：管线照常执行，
    /// 只是不产生 turn 绑定的事实。
    pub(crate) fn require_model_turn(
        &self,
        entry: &'static str,
    ) -> AgentResult<Option<&ModelTurnFacts>> {
        if self.requires_model_turn() {
            return self.require_turn(entry).map(Some);
        }
        Ok(self.turn.as_ref())
    }

    /// 本次执行入口是否**按定义**处于模型 turn 内（模型批次 / 显式模型来源）。
    pub(crate) fn requires_model_turn(&self) -> bool {
        match &self.entry_origin {
            ExecutionEntryOrigin::ModelBatch => true,
            ExecutionEntryOrigin::Single(origin) => {
                matches!(origin, InvocationOrigin::Model { .. })
            }
        }
    }

    /// canonical session 身份；本次执行没有真实 ACP 会话身份时为 `None`。
    ///
    /// `None` 不是「未知的会话」，而是「没有会话身份」：需要交互归属（审批 /
    /// 提问）的入口据此显式拒绝，不得回退到 agent_id 之类非会话身份。
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub(crate) fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    pub(crate) fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub(crate) fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 可见消息快照（提交前视图；不含本批 AI 消息）。
    pub(crate) fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    pub(crate) fn context_usage(&self) -> Option<ContextUsage> {
        self.context_usage
    }

    pub(crate) fn session_resources(&self) -> Option<&Arc<dyn SessionResources>> {
        self.session_resources.as_ref()
    }

    pub(crate) fn resolver(&self) -> &Arc<dyn ToolInvocationResolver> {
        &self.resolver
    }

    pub(crate) fn middleware_chain(&self) -> &MiddlewareChain {
        &self.chain
    }

    pub(crate) fn queue(&self) -> &InvocationQueuePort {
        &self.queue
    }

    /// emit 一条 render 事件（有界通道，满时丢弃并告警）。
    ///
    /// 无事件端口的执行（idle App / Host 入口）不产生任何模型可见事件。
    pub(crate) fn emit_render(&self, event: RenderEvent) {
        if let Some(events) = self.events.as_ref() {
            events.emit_render(event);
        }
    }

    /// 执行入口的取消令牌。
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// 显式记录一次「需要 turn 的事后钩子被跳过」（裁决 R2）。
    ///
    /// 同时写结构化记录（供 outcome 记录消费）与 warn 日志：跳过必须是可见事实，
    /// 不得静默；也不因此把已发生副作用的执行改写成失败。
    pub(crate) fn record_skipped_post_hook(&self, hook: SkippedPostHook) {
        tracing::warn!(
            hook = hook.as_str(),
            session_id = ?self.session_id,
            "post hook skipped: an active model turn is required"
        );
        if let Some(sink) = self.post_hook_skips.as_ref() {
            sink.record(hook);
        }
    }

    /// 「审批不可用（无人可答）」的记录点（裁决 R9）。
    ///
    /// 消费方据此区分「没有用户可以决定」与「用户拒绝」；`None` = 本次执行没有
    /// 记录面，调用方丢弃该事实（不伪造拒绝、也不伪造批准）。
    pub(crate) fn approval_unavailable_sink(&self) -> Option<Arc<ApprovalUnavailableSink>> {
        self.approval_unavailable.clone()
    }

    /// 是否具备 render 事件端口；idle 执行按构造没有（零模型可见事件）。
    #[cfg(test)]
    pub(crate) fn has_render_events(&self) -> bool {
        self.events.is_some()
    }

    /// idle（无活跃 turn）执行入口的构造：turn 事实**不存在**，而不是被清空。
    ///
    /// 触发方（session-owned canonical 入口）按 session 级事实逐项给出输入：
    /// 它没有 `StageContext`（那是**轮次**投影），因此不得借阶段上下文伪造 turn。
    /// 归属由 [`SessionInvocationSource::origin`] 单一派生：App / Host 来源按构造
    /// 映射为 `AppAttachment` / `HostRequest`；未附着的 App 来源没有归属
    ///（后续需要交互的规则据此显式拒绝）。
    ///
    /// 本执行**不产生**模型可见事件（`events: None`）：不冒充模型工具调用、
    /// 不写 `step`、不写 `turn_generation`。
    pub fn from_source(source: SessionInvocationSource) -> Self {
        let attribution = InteractionAttribution::from_invocation_origin(&source.origin);
        Self {
            session_id: source.session_id,
            run_id: source.run_id,
            agent_id: source.agent_id,
            cwd: source.cwd,
            messages: source.messages,
            context_usage: source.context_usage,
            session_resources: source.session_resources,
            resolver: source.resolver,
            chain: source.chain,
            entry_origin: ExecutionEntryOrigin::Single(source.origin),
            attribution,
            turn: None,
            queue: InvocationQueuePort {
                queue: source.queue,
            },
            events: None,
            cancellation: source.cancel,
            post_hook_skips: source.post_hook_skips,
            approval_unavailable: source.approval_unavailable,
        }
    }
}

/// idle（无活跃 turn）执行的会话侧输入（[`InvocationContext::from_source`]）。
///
/// 全部字段都是 session 级事实或端口；**不含** `StageContext`、per-turn
/// `MiddlewareChain` 快照、EventBus：session 级调用入口不得持有 per-turn 对象。
pub struct SessionInvocationSource {
    /// canonical session 身份；`None` = 本次执行没有真实 ACP 会话身份。
    pub session_id: Option<String>,
    /// ReAct run 关联键（metrics / tracing）。
    pub run_id: Option<String>,
    /// 本 session 的 agent 身份（render 事件纽带；idle 执行不发事件）。
    pub agent_id: AgentId,
    /// 执行时刻的工作目录。
    pub cwd: Arc<str>,
    /// 执行时刻的可见消息快照。
    pub messages: Arc<Vec<BaseMessage>>,
    /// 上下文用量快照；无预算或无估算时为 `None`。
    pub context_usage: Option<ContextUsage>,
    /// 会话资源端口。
    pub session_resources: Option<Arc<dyn SessionResources>>,
    /// canonical 目标解析端口。
    pub resolver: Arc<dyn ToolInvocationResolver>,
    /// session 级准入规则链（before-tool 规则；不含 turn 专属阶段）。
    pub chain: Arc<MiddlewareChain>,
    /// 会话队列（hook 输出端口）。
    pub queue: MessageQueue,
    /// 本次执行的 typed 来源（App / Host）。
    pub origin: InvocationOrigin,
    /// 本次执行的取消令牌（唯一中断事实源）。
    pub cancel: CancellationToken,
    /// 事后钩子跳过事实的落点；`None` = 不收集（本入口在无 turn 时会跳过它们）。
    pub post_hook_skips: Option<Arc<PostHookSkipSink>>,
    /// 「审批不可用」事实的落点；`None` = 不收集。
    pub approval_unavailable: Option<Arc<ApprovalUnavailableSink>>,
}

#[cfg(test)]
#[path = "invocation_context_test.rs"]
mod tests;
