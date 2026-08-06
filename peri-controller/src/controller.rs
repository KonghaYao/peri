//! Controller 层控制面宿主（`docs/top-level.md` §6）。
//!
//! 控制面五步：lite params → pick Resources → pick Runtime → run Session → pop events。
//! - [`LiteParams`]：session 标识 / agent 定义引用 / cwd / 初始输入 / 初始消息与
//!   工具集装载（§6）
//! - [`Controller::pick_resources`] / [`Controller::pick_runtime`]：从注入的
//!   Resources / Runtime 取上下文（其余上下文由 Controller 从 Resources 组装注入）
//! - [`Controller::run_session`]：经 Runtime 查映射拿 [`SessionHandle`] 发起执行
//!   （Controller → Runtime 边，§6 run Session）
//! - [`Controller::join_session`] / [`Controller::destroy_session`] / [`Controller::session_ids`]：
//!   会话生命周期面（join 等待自然终止 / §9 六阶段销毁 / 枚举透传 Runtime 映射）
//! - [`Controller::submit_input`]：消息/工具注入面（运行时输入经 Runtime 收口
//!   到 `SessionHandle::submit_input`；初始输入在 LiteParams）
//! - [`Controller::pop_events`] / [`Controller::subscribe`]：事件协议化前分支
//!   （业务事件 → ACP 协议化的出口）；Langfuse bridge 是此分支上的旁路消费者
//!   （§6 观测：bridge 装配在 Controller 侧宿主，不承担 Controller 职责）；
//!   订阅者经 [`Subscription`] 显式注册/退订
//! - [`Controller::cancel`]：按 (session_id, turn_id, attempt_id) 三元组定位并转发
//!   （§6/§9）；幂等判定与取消语义归 Agent 层，本层只定位与转发

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use peri_acp_types::event::{EventMessage, ExecutorEvent};
use peri_acp_types::identity::{CancelRequest, EventEnvelope};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::runtime::UnstampedEvent;
use peri_acp_types::store::ThreadStore;
use peri_agent::tools::ToolDefinition;
use peri_resources::Resources;
use peri_runtime::Runtime;
use tokio::sync::{broadcast, mpsc};

use crate::error::{ControllerError, SubscriptionError};

/// 事件通道容量（弹出队列与订阅广播共用）。
///
/// 交付语义对齐 §9 事件契约：弹出队列为有界通道（满时丢弃，对应 Critical
/// 交付类）；订阅广播对慢消费者 lagging（对应 Broadcast 交付类）。
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// agent 定义引用（§6 lite params）。
///
/// 引用 agent 定义（agm 命名空间 / 内置定义名），解析归 Agent 层；
/// Controller 只作为 lite params 的组成部分透传。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef(String);

impl AgentRef {
    /// 以定义名构造引用。
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// 定义名字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// lite params（§6）：控制面第一步的会话启动参数。
///
/// 仅承载会话启动的最小参数集（session 标识、agent 定义引用、cwd、初始输入、
/// 初始消息与工具集装载）；其余上下文由 Controller 从 Resources 组装注入。
/// 初始消息/工具集的消费方为 Agent 层 session 工厂（L5），本类型为透传声明
/// （接口先于实现；`tools` 字段的 `ToolDefinition` 来源待分波 2 裁定）。
#[derive(Debug, Clone)]
pub struct LiteParams {
    /// session 标识（thread_id）。
    pub session_id: String,
    /// agent 定义引用。
    pub agent_ref: AgentRef,
    /// 工作目录。
    pub cwd: PathBuf,
    /// 初始输入（首条 user 消息；无输入时为 None）。
    pub initial_input: Option<String>,
    /// 初始消息装载（会话启动时注入的初始消息；默认空）。
    pub initial_messages: Vec<MessageContent>,
    /// 初始工具集装载（会话启动时注册的额外工具；默认空）。
    pub tools: Vec<ToolDefinition>,
}

impl LiteParams {
    /// 构造 lite params。
    pub fn new(
        session_id: impl Into<String>,
        agent_ref: AgentRef,
        cwd: impl Into<PathBuf>,
        initial_input: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            agent_ref,
            cwd: cwd.into(),
            initial_input,
            initial_messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// 带初始消息装载构造。
    pub fn with_initial_messages(mut self, messages: Vec<MessageContent>) -> Self {
        self.initial_messages = messages;
        self
    }

    /// 带初始工具集装载构造。
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }
}

/// 事件订阅句柄：[`Controller::subscribe`] 的显式返回类型。
///
/// 交付语义（§9 事件契约 Broadcast 类）：慢消费者 lagging
/// （[`SubscriptionError::Lagged`]，错过条数可观测）；事件流终止（广播通道
/// 关闭）报 [`SubscriptionError::Closed`]。
///
/// 退订语义 = 接收端 drop（broadcast 在无订阅者时不再分发），无需
/// Controller 侧簿记；[`Subscription::unsubscribe`] 是显式退订入口。
#[derive(Debug)]
pub struct Subscription {
    receiver: broadcast::Receiver<EventMessage>,
}

impl Subscription {
    /// 接收下一条事件。
    pub async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        self.receiver.recv().await.map_err(|err| match err {
            broadcast::error::RecvError::Lagged(skipped) => SubscriptionError::Lagged(skipped),
            broadcast::error::RecvError::Closed => SubscriptionError::Closed,
        })
    }

    /// 非阻塞取一条事件（空返回 `None`）。
    ///
    /// 用于事件源结束后排干广播中的在途事件（`try_recv` 直到 `Empty`），
    /// 语义与 `mpsc::Receiver::try_recv` 一致。
    pub fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        match self.receiver.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                Err(SubscriptionError::Lagged(skipped))
            }
            Err(broadcast::error::TryRecvError::Closed) => Err(SubscriptionError::Closed),
        }
    }

    /// 显式退订：drop 接收端，此后不再收到事件。
    pub fn unsubscribe(self) {}
}

/// Controller 层宿主：业务操作的组合入口（控制面）。
///
/// 持有：
/// - sessions 存储通道（Thread/transcript = 持久真相，§9），由部署装配点注入
///   （Resources 侧打开后传入）
/// - Runtime 编排器（pick Runtime 的目标源）：部署装配点经
///   [`Controller::with_runtime`] 注入；缺省为空实例（生产接线随 L5 落地）
/// - Resources 门面（pick Resources 的目标源）：部署装配点经
///   [`Controller::with_resources`] 注入；缺省未注入（None）
/// - 装配注入端口（pick 目标源）：mcp 池 / cron 调度器 / 工具检索索引 /
///   LSP 服务器配置，宿主装配点构造具体实现后 upcast 注入（3.0 批 2 波 2；
///   消费方为执行装配，随 L5 落位）
/// - 事件协议化前分支（弹出队列 + 订阅广播）
pub struct Controller {
    /// 持久化存储通道（等价包装 `ThreadStore`，不改变其 trait 语义）。
    sessions: Arc<dyn ThreadStore>,
    /// 多 session 编排器（§3；注入后为共享实例，非本层新建）。
    runtime: RwLock<Arc<Runtime>>,
    /// 外部系统资源门面（§5；以 context 形式提供给 Controller）。
    resources: RwLock<Option<Resources>>,
    /// MCP 客户端池端口（pick 目标源；缺省未注入）。
    mcp_pool: RwLock<Option<Arc<dyn peri_acp_types::ports::McpPoolPort>>>,
    /// Cron 调度器端口（pick 目标源；缺省未注入）。
    cron_scheduler: RwLock<Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>>>,
    /// 工具检索索引端口（pick 目标源；缺省未注入）。
    tool_search: RwLock<Option<Arc<dyn peri_acp_types::ports::ToolSearchPort>>>,
    /// LSP 服务器配置（pick 目标源；缺省空）。
    lsp_servers: RwLock<Vec<peri_acp_types::lsp::LspServerConfig>>,
    /// 弹出队列发送端（pop_events 消费；有界满丢弃）。
    events_tx: mpsc::Sender<EventMessage>,
    /// 弹出队列接收端（控制面第五步 pop events）。
    events_rx: Mutex<mpsc::Receiver<EventMessage>>,
    /// 订阅广播（subscribe 分发；慢消费者 lagging）。
    subscribers: broadcast::Sender<EventMessage>,
}

impl Controller {
    /// 以存储通道构造 Controller。
    ///
    /// Runtime / Resources / 装配注入端口由部署装配点（Resources 打开后、
    /// Runtime 建立后）经对应 `with_*` 注入；本构造函数保持既有调用点兼容。
    pub fn new(sessions: Arc<dyn ThreadStore>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (subscribers, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sessions,
            runtime: RwLock::new(Arc::new(Runtime::new())),
            resources: RwLock::new(None),
            mcp_pool: RwLock::new(None),
            cron_scheduler: RwLock::new(None),
            tool_search: RwLock::new(None),
            lsp_servers: RwLock::new(Vec::new()),
            events_tx,
            events_rx: Mutex::new(events_rx),
            subscribers,
        }
    }

    /// 注入 Runtime 编排器（pick Runtime 的目标源；部署装配点调用）。
    pub fn with_runtime(self, runtime: Arc<Runtime>) -> Self {
        *self.runtime.write() = runtime;
        self
    }

    /// 注入 Resources 门面（pick Resources 的目标源；部署装配点在
    /// `Resources::open()` 后调用）。
    pub fn with_resources(self, resources: Resources) -> Self {
        *self.resources.write() = Some(resources);
        self
    }

    /// 注入 MCP 客户端池端口（pick MCP 池的目标源；宿主装配点构造具体
    /// `McpClientPool` 后 upcast 注入）。
    pub fn with_mcp_pool(
        self,
        mcp_pool: Option<Arc<dyn peri_acp_types::ports::McpPoolPort>>,
    ) -> Self {
        *self.mcp_pool.write() = mcp_pool;
        self
    }

    /// 注入 Cron 调度器端口（pick cron 调度器的目标源；宿主装配点构造
    /// `CronSchedulerPortHandle` 后注入）。
    pub fn with_cron_scheduler(
        self,
        cron_scheduler: Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>>,
    ) -> Self {
        *self.cron_scheduler.write() = cron_scheduler;
        self
    }

    /// 注入工具检索索引端口（pick 工具检索索引的目标源；宿主装配点构造
    /// `ToolSearchIndex` 后 upcast 注入）。
    pub fn with_tool_search(
        self,
        tool_search: Option<Arc<dyn peri_acp_types::ports::ToolSearchPort>>,
    ) -> Self {
        *self.tool_search.write() = tool_search;
        self
    }

    /// 注入 LSP 服务器配置（pick LSP 配置的目标源）。
    pub fn with_lsp_servers(self, lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>) -> Self {
        *self.lsp_servers.write() = lsp_servers;
        self
    }

    /// pick MCP 客户端池（控制面资源取用；未注入返回 `None`）。
    pub fn pick_mcp_pool(&self) -> Option<Arc<dyn peri_acp_types::ports::McpPoolPort>> {
        self.mcp_pool.read().clone()
    }

    /// pick Cron 调度器（控制面资源取用；未注入返回 `None`）。
    pub fn pick_cron_scheduler(&self) -> Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>> {
        self.cron_scheduler.read().clone()
    }

    /// pick 工具检索索引（控制面资源取用；未注入返回 `None`）。
    pub fn pick_tool_search(&self) -> Option<Arc<dyn peri_acp_types::ports::ToolSearchPort>> {
        self.tool_search.read().clone()
    }

    /// pick LSP 服务器配置（控制面资源取用；未注入返回空）。
    pub fn pick_lsp_servers(&self) -> Vec<peri_acp_types::lsp::LspServerConfig> {
        self.lsp_servers.read().clone()
    }

    /// Controller 侧 sessions 访问通道。
    ///
    /// 返回存储句柄供业务操作使用；语义与 `ThreadStore` 完全等价，仅改变访问路径。
    pub fn sessions(&self) -> Arc<dyn ThreadStore> {
        Arc::clone(&self.sessions)
    }

    /// pick Resources（控制面第二步）：取注入的 Resources 门面。
    ///
    /// 未注入（部署装配点尚未提供）时返回 `None`；组装注入上下文的职责
    /// 随 L5 装配落位。
    pub fn pick_resources(&self) -> Option<Resources> {
        self.resources.read().clone()
    }

    /// pick Runtime（控制面第三步）：取注入的 Runtime 编排器引用。
    pub fn pick_runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.runtime.read())
    }

    /// run Session（控制面第四步）：经 Runtime 查映射拿 `SessionHandle` 发起执行。
    ///
    /// 只发起不解释：执行结果（含终态）由 Agent 层产生，错误经 Runtime 边界
    /// 包 context 为 [`ControllerError::RunFailed`]。
    pub async fn run_session(&self, session_id: &str) -> Result<(), ControllerError> {
        let runtime = Arc::clone(&self.runtime.read());
        runtime
            .run(session_id)
            .await
            .map_err(|err| ControllerError::RunFailed(session_id.to_string(), err))
    }

    /// 注册会话运行句柄（run Session 的前置）：把本轮执行句柄注册进
    /// Runtime 映射（§3 `session_id -> SessionHandle`），供
    /// [`Controller::run_session`] 查映射发起。
    ///
    /// 注册语义 = 注册或替换（[`Runtime::register_or_replace`]）：同一 session
    /// 每轮执行发起前调用本方法刷新句柄（不递增 epoch / 不重置 seq），
    /// 未注册时等价首次注册。句柄实现方为 ACP 层执行薄壳（过渡）或
    /// Agent 层 session 工厂（L5）。
    pub fn register_session<H>(&self, session_id: &str, handle: Arc<H>)
    where
        H: peri_acp_types::runtime::SessionHandle + 'static,
    {
        self.runtime.read().register_or_replace(session_id, handle);
    }

    /// cancel 转发（§6/§9）：按 (session_id, turn_id, attempt_id) 三元组
    /// 定位并转发，幂等判定与取消语义归 Agent 层（本层只定位与转发）。
    ///
    /// 定位依据为请求携带的三元组（`CancelRequest.identity`）；转发失败
    /// （session 未注册等）包 context 为 [`ControllerError::CancelFailed`]。
    pub fn cancel(&self, request: &CancelRequest) -> Result<(), ControllerError> {
        self.runtime
            .read()
            .cancel(request)
            .map_err(|err| ControllerError::CancelFailed(request.identity.session_id.clone(), err))
    }

    /// 会话枚举（对应 ACP list_sessions 语义）：透传 Runtime 映射的
    /// session_id 列表。
    ///
    /// 无顺序保证（Runtime 簿记为 HashMap）；list_sessions 需要的元数据
    /// （标题/时间等）经存储通道（[`Controller::sessions`]）合并。
    pub fn session_ids(&self) -> Vec<String> {
        self.runtime.read().session_ids()
    }

    /// 该会话是否已注册（Runtime 映射命中）。
    pub fn contains_session(&self, session_id: &str) -> bool {
        self.runtime.read().contains(session_id)
    }

    /// join 会话：等待 session 结束（带 deadline）。
    ///
    /// 返回 `true` = deadline 内结束；`false` = 超时（调用方决定 abort 或
    /// 继续等待；销毁路径的超时 abort 由 [`Controller::destroy_session`] 编排）。
    /// 未注册 session 包 context 为 [`ControllerError::JoinFailed`]。
    pub async fn join_session(
        &self,
        session_id: &str,
        deadline: Duration,
    ) -> Result<bool, ControllerError> {
        let runtime = Arc::clone(&self.runtime.read());
        runtime
            .join(session_id, deadline)
            .await
            .map_err(|err| ControllerError::JoinFailed(session_id.to_string(), err))
    }

    /// 销毁会话（§9 session 销毁顺序契约，经 Runtime 编排）：
    ///
    /// 停收新输入 → 取消 owned tasks → join（带 deadline）→ 超时 abort →
    /// 持久化事务收束 → drain 事件 → 移除映射。drain 出的补打事件经
    /// [`Controller::publish`] 双投递（弹出队列 + 订阅分支），并作为返回值
    /// 交给调用方（ACP 可继续经 pop_events / 订阅消费）。
    ///
    /// 持久化失败时映射保留（重试安全），包 context 为
    /// [`ControllerError::DestroyFailed`]。
    pub async fn destroy_session(
        &self,
        session_id: &str,
        join_deadline: Duration,
    ) -> Result<Vec<EventEnvelope>, ControllerError> {
        let runtime = Arc::clone(&self.runtime.read());
        let drained = runtime
            .destroy(session_id, join_deadline)
            .await
            .map_err(|err| ControllerError::DestroyFailed(session_id.to_string(), err))?;
        for envelope in &drained {
            self.publish(envelope.clone());
        }
        Ok(drained)
    }

    /// 注入运行时输入（消息/工具注入面）：经 Runtime 查映射透传
    /// `SessionHandle::submit_input`（运行期输入收口；初始输入在 LiteParams）。
    ///
    /// 未注册 session 或句柄注入失败包 context 为 [`ControllerError::InjectFailed`]
    /// （Agent 侧细节错误经 Runtime 边界 anyhow 穿透）。
    pub fn submit_input(
        &self,
        session_id: &str,
        input: MessageContent,
    ) -> Result<(), ControllerError> {
        let runtime = Arc::clone(&self.runtime.read());
        runtime
            .submit_input(session_id, input)
            .map_err(|err| ControllerError::InjectFailed(session_id.to_string(), err))
    }

    /// 事件投递入口（协议化前分支）：宿主把 Runtime 聚合补打后的事件投进
    /// Controller，同时分发给弹出队列与全部订阅者。
    ///
    /// 交付语义（§9 事件契约）：弹出队列有界满丢弃（Critical 类）；
    /// 订阅广播慢消费者 lagging（Broadcast 类）。
    ///
    /// 本方法接收**身份事件**（销毁路径 drain 出的事件无 v1 payload），
    /// 包装为 [`EventMessage`]（`event: None`）投递；业务事件发射请用
    /// [`Controller::publish_event`]。
    pub fn publish(&self, envelope: EventEnvelope) {
        self.publish_message(EventMessage::new(envelope, None));
    }

    /// 事件发射入口（协议化前分支，事件三层化统一出口）：ACP 侧发射点
    /// （Agent EventBus 消费侧）把 Agent 层未补打事件（身份 + v1 payload）
    /// 经本方法发射——Controller 经 Runtime 补打 `session_id` / `session_seq`
    /// （§9 事件契约）后双投递（弹出队列 + 订阅广播）。
    ///
    /// 交付语义同 [`Controller::publish`]：弹出队列有界满丢弃（Critical 类）；
    /// 订阅广播慢消费者 lagging（Broadcast 类）。未注册 session（迟到事件 /
    /// 销毁后事件）无法补打，降级为发射方提供的身份直接投递（不 panic）。
    pub fn publish_event(&self, session_id: &str, source: &UnstampedEvent, event: ExecutorEvent) {
        let runtime = Arc::clone(&self.runtime.read());
        let envelope = match runtime.stamp(session_id, source) {
            Ok(stamped) => stamped,
            Err(_) => EventEnvelope::new(
                session_id.to_string(),
                peri_acp_types::identity::SessionEpoch::initial(),
                source.turn_id.clone(),
                source.agent_id.clone(),
                peri_acp_types::identity::SessionSeq::initial(),
                source.delivery_class,
            ),
        };
        self.publish_message(EventMessage::new(envelope, Some(event)));
    }

    /// 双投递（弹出队列 + 订阅广播）的内部实现。
    fn publish_message(&self, msg: EventMessage) {
        let _ = self.events_tx.try_send(msg.clone());
        let _ = self.subscribers.send(msg);
    }

    /// 订阅协议化前分支：向 ACP 提供事件流（ACP 协议化映射的输入），
    /// Langfuse bridge 等旁路消费者以同一分支订阅（§6 观测：旁路不参与业务链路）。
    ///
    /// 返回显式订阅句柄 [`Subscription`]；退订 = 句柄 drop 或
    /// [`Subscription::unsubscribe`]（broadcast 语义，无 Controller 侧簿记）。
    pub fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.subscribers.subscribe(),
        }
    }

    /// pop events（控制面第五步）：按投递序弹出队列中全部待处理事件。
    pub fn pop_events(&self) -> Vec<EventMessage> {
        let mut rx = self.events_rx.lock();
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    }
}

#[cfg(test)]
#[path = "controller_test.rs"]
mod tests;
