//! Controller 层控制面宿主（`docs/top-level.md` §6）。
//!
//! 控制面五步：lite params → pick Resources → pick Runtime → run Session → pop events。
//! - [`LiteParams`]：session 标识 / agent 定义引用 / cwd / 初始输入（§6）
//! - [`Controller::pick_resources`] / [`Controller::pick_runtime`]：从注入的
//!   Resources / Runtime 取上下文（其余上下文由 Controller 从 Resources 组装注入）
//! - [`Controller::run_session`]：经 Runtime 查映射拿 [`SessionHandle`] 发起执行
//!   （Controller → Runtime 边，§6 run Session）
//! - [`Controller::pop_events`] / [`Controller::subscribe`]：事件协议化前分支
//!   （业务事件 → ACP 协议化的出口）；Langfuse bridge 是此分支上的旁路消费者
//!   （§6 观测：bridge 装配在 Controller 侧宿主，不承担 Controller 职责）
//! - [`Controller::cancel`]：按 (session_id, turn_id, attempt_id) 三元组定位并转发
//!   （§6/§9）；幂等判定与取消语义归 Agent 层，本层只定位与转发

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use peri_acp_types::identity::{CancelRequest, EventEnvelope};
use peri_acp_types::store::ThreadStore;
use peri_resources::Resources;
use peri_runtime::Runtime;
use tokio::sync::{broadcast, mpsc};

use crate::error::ControllerError;

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
/// 仅承载会话启动的最小参数集（session 标识、agent 定义引用、cwd、初始输入）；
/// 其余上下文由 Controller 从 Resources 组装注入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteParams {
    /// session 标识（thread_id）。
    pub session_id: String,
    /// agent 定义引用。
    pub agent_ref: AgentRef,
    /// 工作目录。
    pub cwd: PathBuf,
    /// 初始输入（首条 user 消息；无输入时为 None）。
    pub initial_input: Option<String>,
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
        }
    }
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
/// - 事件协议化前分支（弹出队列 + 订阅广播）
pub struct Controller {
    /// 持久化存储通道（等价包装 `ThreadStore`，不改变其 trait 语义）。
    sessions: Arc<dyn ThreadStore>,
    /// 多 session 编排器（§3；注入后为共享实例，非本层新建）。
    runtime: RwLock<Arc<Runtime>>,
    /// 外部系统资源门面（§5；以 context 形式提供给 Controller）。
    resources: RwLock<Option<Resources>>,
    /// 弹出队列发送端（pop_events 消费；有界满丢弃）。
    events_tx: mpsc::Sender<EventEnvelope>,
    /// 弹出队列接收端（控制面第五步 pop events）。
    events_rx: Mutex<mpsc::Receiver<EventEnvelope>>,
    /// 订阅广播（subscribe 分发；慢消费者 lagging）。
    subscribers: broadcast::Sender<EventEnvelope>,
}

impl Controller {
    /// 以存储通道构造 Controller。
    ///
    /// Runtime / Resources 由部署装配点（Resources 打开后、Runtime 建立后）
    /// 经 [`Controller::with_runtime`] / [`Controller::with_resources`] 注入；
    /// 本构造函数保持既有调用点兼容。
    pub fn new(sessions: Arc<dyn ThreadStore>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (subscribers, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sessions,
            runtime: RwLock::new(Arc::new(Runtime::new())),
            resources: RwLock::new(None),
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

    /// 事件投递入口（协议化前分支）：宿主把 Runtime 聚合补打后的事件投进
    /// Controller，同时分发给弹出队列与全部订阅者。
    ///
    /// 交付语义（§9 事件契约）：弹出队列有界满丢弃（Critical 类）；
    /// 订阅广播慢消费者 lagging（Broadcast 类）。
    pub fn publish(&self, envelope: EventEnvelope) {
        let _ = self.events_tx.try_send(envelope.clone());
        let _ = self.subscribers.send(envelope);
    }

    /// 订阅协议化前分支：向 ACP 提供事件流（ACP 协议化映射的输入），
    /// Langfuse bridge 等旁路消费者以同一分支订阅（§6 观测：旁路不参与业务链路）。
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.subscribers.subscribe()
    }

    /// pop events（控制面第五步）：按投递序弹出队列中全部待处理事件。
    pub fn pop_events(&self) -> Vec<EventEnvelope> {
        let mut rx = self.events_rx.lock();
        let mut out = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            out.push(envelope);
        }
        out
    }
}

#[cfg(test)]
#[path = "controller_test.rs"]
mod tests;
