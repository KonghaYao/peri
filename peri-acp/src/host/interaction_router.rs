//! session-owned 交互路由：按**可信归属**选择审批 / 提问的在线 endpoint。
//!
//! 设计 §3.3 / §1 #1-#2：`SessionInteractionRouter` 是 session 级对象（随
//! `SessionEnvironment` 存活、跨轮不重建），持**有界**在线 endpoint 表、
//! 当前模型轮 endpoint 与 View 附件路由表；每一轮的固定 broker 不再被缓存
//! 成 session 级权威——endpoint 随连接替换，连接关闭即 `retire`。
//!
//! 归属 → endpoint 的选择规则：
//!
//! | 归属 | endpoint 来源 |
//! |---|---|
//! | [`TrustedInteractionOwner::Model`] | prompt 轮发布（唯一发布者：`host/prompt.rs`），轮末清除 |
//! | [`TrustedInteractionOwner::App`] | View 附件路由表，**附件纪元必须相等**（旧纪元迟到批准丢弃） |
//! | [`TrustedInteractionOwner::Host`] | 请求作用域注入的连接（见 [`scope_host_request_connection`]） |
//!
//! 投递失败 ⇒ 立即 `Unanswered{cause}`，且**原因必须与事实相符**（裁决 D3 的附加
//! 要求；独立审查「中 2」即「同一原因承载多种事实」的诚实性缺口）：
//!
//! - [`UnansweredCause::NoOnlineEndpoint`]：本端**一个在线 endpoint 都没有**；
//! - [`UnansweredCause::OwnerUnmatched`]：本端**有**在线 endpoint，但没有一个
//!   符合本次归属（归属会话不符 / 本轮未发布 / 附件不在表内 / 纪元不符 / 承载
//!   连接未注册或已离线 / Host 无请求作用域且多端点歧义）；
//! - [`UnansweredCause::AttachmentDetached`]：附件与纪元都成立，但该 View 已
//!   detach（页面卸载 / 连接退休），有界窗口内未重挂接。
//!
//! 三者都不得降级为 `PermissionMiddleware::disabled()`、不得自动批准、不得伪造成
//! 「用户拒绝」。
//!
//! 转发实现复用既有 [`AcpTransportBroker`]（同一 session 门、同一审批 / 提问
//! 编解码与错误映射），本模块不复制第二套 wire 语义。
//!
//! 依赖方向：本文件只用 `peri-acp` 内部 transport 与 `peri-acp-types` 契约，
//! **不引用 `peri_middlewares`**（`scripts/import-exemptions.conf` 边 2 无需新增豁免）。

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::interaction::{
    AttachmentId, ConnectionEpoch, InteractionContext, InteractionResponse,
    TrustedInteractionOwner, UnansweredCause, UserInteractionBroker,
};
use peri_acp_types::mcp_apps_facade::{
    AttachmentDetachReason, ClientViewId, RequestNamespaceId, DETACH_REATTACH_WINDOW_MS,
    MAX_ATTACHMENTS_PER_SESSION,
};

use crate::broker::AcpTransportBroker;
use crate::transport::{AcpRequestBridge, AcpTransport};

/// 在线 endpoint 表上限（设计 §1 #2「有界（≤8）」）。
///
/// 满表时**拒绝**新连接注册（返回 [`EndpointTableFull`]），不淘汰仍在线的
/// endpoint：淘汰意味着把一个在线连接的审批静默改投到另一个连接。
pub const MAX_ONLINE_ENDPOINTS: usize = 8;

/// endpoint 表满：调用方必须显式拒绝（fail-closed），不得静默替换在线 endpoint。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointTableFull;

impl std::fmt::Display for EndpointTableFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session interaction endpoint table is full ({MAX_ONLINE_ENDPOINTS})"
        )
    }
}

impl std::error::Error for EndpointTableFull {}

/// 一个在线交互 endpoint（= 一条 ACP 连接的可替换投递面）。
///
/// `connection_epoch` 是**连接级身份**（首次注册为 [`ConnectionEpoch::initial`]）；
/// **附件纪元不在本类型上**：View 附件的纪元权威是 [`SessionAttachment::epoch`]
/// （设计 §5：附件纪元以 `SessionAttachment.epoch` 为唯一权威）。
pub struct SessionInteractionEndpoint {
    connection_id: String,
    connection_epoch: ConnectionEpoch,
    transport: Arc<dyn AcpTransport>,
    online: Arc<AtomicBool>,
}

impl SessionInteractionEndpoint {
    /// 构造在线 endpoint（唯一在线事实源是 [`Self::retire`] 之前的 `online` 位）。
    pub fn new(
        connection_id: impl Into<String>,
        connection_epoch: ConnectionEpoch,
        transport: Arc<dyn AcpTransport>,
    ) -> Self {
        Self {
            connection_id: connection_id.into(),
            connection_epoch,
            transport,
            online: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn connection_epoch(&self) -> ConnectionEpoch {
        self.connection_epoch
    }

    pub fn transport(&self) -> &Arc<dyn AcpTransport> {
        &self.transport
    }

    /// 在线位：`false` ⇒ 该连接不再是可投递 endpoint（审批随之 fail-closed）。
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst)
    }

    /// 置离线（连接关闭 / 退出）。幂等；已发出的请求不被取消（由 invocation
    /// owner 收尾，设计 §2）。
    pub fn retire(&self) {
        self.online.store(false, Ordering::SeqCst);
    }

    /// 该 endpoint 的 transport 是否就是给定 transport（同一 `Arc` 实例）。
    ///
    /// ACP 服务实例内 transport 与连接 1:1（`ConnectionContext` 每 server 一个），
    /// 模型轮由 prompt 装配点按它**正在使用的** transport 发布，因此这里按
    /// `Arc::ptr_eq` 判定身份，不做字符串 / 端口自述比对。
    pub fn is_transport(&self, transport: &Arc<dyn AcpTransport>) -> bool {
        Arc::ptr_eq(&self.transport, transport)
    }
}

impl std::fmt::Debug for SessionInteractionEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInteractionEndpoint")
            .field("connection_id", &self.connection_id)
            .field("connection_epoch", &self.connection_epoch)
            .field("online", &self.is_online())
            .finish()
    }
}

/// 附着请求：View 身份 + 承载连接。
///
/// - `attachment_id`：请求域身份（**调用方铸造**：新 `apps/attach` 由 façade 分配；
///   旧 `peri/mcp/open` 别名用其合成的附件身份）。本层校验它在表内唯一，**不得
///   以同 ID 重建**（重挂接走 [`SessionInteractionRouter::reattach_view`]）；
/// - `namespace`（请求去重域）与 `epoch`（附件纪元）由**本层分配**：纪元是附件
///   投递资格的唯一权威，只增不减。
pub struct ViewAttachRequest {
    pub attachment_id: AttachmentId,
    /// App 实例身份（= record 身份）。
    pub app_id: String,
    /// 客户端页面身份：同一 View 的重挂接以 `(app_id, view_key)` 稳定。
    pub view_key: ClientViewId,
    /// 承载该 View 的**可信连接**（本层注册过的在线 endpoint）。
    pub connection_id: String,
}

/// 附件投递状态：附着（可投递）或 detach（有界重挂接窗口内保留身份）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentState {
    /// 附着中：可向该连接投递交互。
    Attached {
        /// 承载连接身份。
        connection_id: String,
    },
    /// 已 detach：身份与纪元保留（窗口内可重挂接），但没有可投递的连接。
    Detached {
        /// detach 时刻。
        since: Instant,
        /// 重挂接截止时刻（`since + DETACH_REATTACH_WINDOW`）；到期即不可再用。
        reattach_deadline: Instant,
    },
}

/// 一个 View 的连接附着（session 级路由表的条目）。
///
/// `epoch` 是附件投递资格的唯一权威（重挂接递增，旧纪元迟到交互必须被丢弃）；
/// 与 endpoint 的 `connection_epoch`（连接级身份）**不是同一事实**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAttachment {
    attachment_id: AttachmentId,
    app_id: String,
    view_key: ClientViewId,
    namespace: RequestNamespaceId,
    epoch: ConnectionEpoch,
    state: AttachmentState,
}

// 读面（身份 / namespace / 纪元 / 状态）：`SessionAppsFacade`（S6）据此构造
// `AttachmentBinding` 与判决——生产调用者已就位，故不再需要 `allow(dead_code)`。
impl SessionAttachment {
    pub fn attachment_id(&self) -> &AttachmentId {
        &self.attachment_id
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn view_key(&self) -> &ClientViewId {
        &self.view_key
    }

    /// 请求去重域（ledger 的 namespace 权威值）。
    pub fn namespace(&self) -> &RequestNamespaceId {
        &self.namespace
    }

    /// 附件纪元（投递资格的唯一权威）。
    pub fn epoch(&self) -> ConnectionEpoch {
        self.epoch
    }

    /// 附件状态机的读面（测试观察面）。
    ///
    /// 生产判定不用它：投递资格看 [`Self::is_deliverable`]，承载连接看
    /// [`Self::connection_id`]（二者是本模块给出的判决面）。保留本访问器是为了让
    /// D6/到期清理的用例能直接断言「detach 而非销毁」的状态迁移。
    #[allow(dead_code)]
    pub fn state(&self) -> &AttachmentState {
        &self.state
    }

    /// 当前承载连接（detach 中为 `None`：没有可投递的连接）。
    pub fn connection_id(&self) -> Option<&str> {
        match &self.state {
            AttachmentState::Attached { connection_id } => Some(connection_id),
            AttachmentState::Detached { .. } => None,
        }
    }

    /// 是否仍可投递（附着中）。
    pub fn is_deliverable(&self) -> bool {
        matches!(self.state, AttachmentState::Attached { .. })
    }
}

/// 附着被拒的原因（显式承载，调用方不得静默忽略）。
///
/// 生产调用点是 `apps/attach`（`SessionAppsFacade::attach_identity` → 本层），
/// 消费者按它映射稳定错误键（不静默忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRejection {
    /// 承载连接不是本会话的在线 endpoint（未注册 / 已离线）。
    EndpointUnavailable,
    /// 附件表已满（上限 `MAX_ATTACHMENTS_PER_SESSION`）：显式拒绝，不淘汰在线附件。
    TableFull,
    /// 该附件身份已在表内（附着中或 detach 窗口内）：不得以同 ID 重建。
    AttachmentExists,
    /// 未知附件（从未附着，或 detach 窗口已到期并被清理）。
    UnknownAttachment,
}

impl std::fmt::Display for AttachRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EndpointUnavailable => {
                f.write_str("attachment connection is not an online endpoint of this session")
            }
            Self::TableFull => write!(
                f,
                "session attachment table is full ({MAX_ATTACHMENTS_PER_SESSION})"
            ),
            Self::AttachmentExists => {
                f.write_str("attachment identity already exists (reattach instead)")
            }
            Self::UnknownAttachment => f.write_str("unknown attachment"),
        }
    }
}

impl std::error::Error for AttachRejection {}

/// `detach` 的判决（幂等；三种结果在类型上可区分）。
///
/// 生产调用点是 `apps/detach`（`SessionAppsFacade::detach` → 本层），消费者按它
/// 映射 wire 判决（不静默忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachVerdict {
    /// 本次调用发生了状态转移（附着 → detach）。
    Detached,
    /// 已是 detach 状态（窗口内），本次调用无状态转移。
    AlreadyDetached,
    /// 未知附件（从未附着 / 窗口已到期）：不创建条目。
    UnknownAttachment,
}

/// 当前模型轮 endpoint 的发布凭据（单次发布；只清除自己发布的那一份）。
///
/// 为什么需要凭据：同一 session 上两个连接可能各有在途 prompt（per-session
/// prompt lock 只序列化**执行**，挂在锁上的发布点不保证互斥）。没有凭据的
/// `clear()` 会把别人的轮次 endpoint 清掉；本类型让「清除」只能撤销自己那次
/// 发布，且后来的发布**取代**先前的（先发布者的审批随之 `NoOnlineEndpoint`，
/// 是 fail-closed，不会跨连接串线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTurnToken(u64);

/// 模型轮 endpoint 的发布守卫：`drop` 即清除（轮末 / 提前返回都覆盖）。
///
/// 只清除**自己那次**发布：后来的发布者（同 session 另一连接的在途 prompt）
/// 拥有当前 endpoint，本守卫不得把它清掉。
pub struct ModelTurnGuard {
    router: Arc<SessionInteractionRouter>,
    token: ModelTurnToken,
}

impl Drop for ModelTurnGuard {
    fn drop(&mut self) {
        self.router.clear_model_turn(self.token);
    }
}

#[derive(Default)]
struct RouterState {
    endpoints: HashMap<String, SessionInteractionEndpoint>,
    /// 当前模型轮 endpoint：`(connection_id, token)`。
    model_turn: Option<(String, ModelTurnToken)>,
    /// View 附件路由表（`MAX_ATTACHMENTS_PER_SESSION` 上限；detach 条目在窗口内
    /// 保留身份，到期惰性清理）。
    attachments: HashMap<AttachmentId, SessionAttachment>,
    next_token: u64,
}

impl RouterState {
    /// 清理**已到期**的 detach 条目（惰性；无后台任务）。返回清理条数。
    ///
    /// 到期后同 ID 不再是「可重挂接的附件」，而是未知附件：调用方必须以新身份
    /// 重新附着（不得以同 ID 重建——namespace / 序号权威随之作废）。
    fn expire_detached(&mut self, now: Instant) -> usize {
        let before = self.attachments.len();
        self.attachments
            .retain(|_, attachment| match &attachment.state {
                AttachmentState::Attached { .. } => true,
                AttachmentState::Detached {
                    reattach_deadline, ..
                } => now < *reattach_deadline,
            });
        before - self.attachments.len()
    }

    /// 附着新 View（身份 / namespace / 纪元分配）。
    fn attach_view(
        &mut self,
        request: ViewAttachRequest,
        now: Instant,
    ) -> Result<SessionAttachment, AttachRejection> {
        self.expire_detached(now);
        if !self
            .endpoints
            .get(&request.connection_id)
            .is_some_and(SessionInteractionEndpoint::is_online)
        {
            return Err(AttachRejection::EndpointUnavailable);
        }
        if self.attachments.contains_key(&request.attachment_id) {
            // 同 ID 重建会把两个请求域混成一个：显式拒绝（重挂接走 `reattach_view`）。
            return Err(AttachRejection::AttachmentExists);
        }
        if self.attachments.len() as u64 >= MAX_ATTACHMENTS_PER_SESSION {
            return Err(AttachRejection::TableFull);
        }
        let attachment = SessionAttachment {
            attachment_id: request.attachment_id,
            app_id: request.app_id,
            view_key: request.view_key,
            namespace: RequestNamespaceId::new(),
            epoch: ConnectionEpoch::initial(),
            state: AttachmentState::Attached {
                connection_id: request.connection_id,
            },
        };
        self.attachments
            .insert(attachment.attachment_id.clone(), attachment.clone());
        Ok(attachment)
    }

    /// 重挂接（同一附件身份换承载连接）：**纪元递增**，旧纪元随即失效。
    fn reattach_view(
        &mut self,
        attachment_id: &AttachmentId,
        connection_id: &str,
        now: Instant,
    ) -> Result<SessionAttachment, AttachRejection> {
        self.expire_detached(now);
        if !self
            .endpoints
            .get(connection_id)
            .is_some_and(SessionInteractionEndpoint::is_online)
        {
            return Err(AttachRejection::EndpointUnavailable);
        }
        let attachment = self
            .attachments
            .get_mut(attachment_id)
            .ok_or(AttachRejection::UnknownAttachment)?;
        attachment.epoch = attachment.epoch.next();
        attachment.state = AttachmentState::Attached {
            connection_id: connection_id.to_string(),
        };
        Ok(attachment.clone())
    }
}

/// session 级交互路由（实现 [`UserInteractionBroker`]）。
pub struct SessionInteractionRouter {
    session_id: String,
    /// session-owned 交互互斥门（与 `SessionState.interaction_gate` 同一 `Arc`）。
    gate: Arc<tokio::sync::Mutex<()>>,
    timeout: Option<Duration>,
    state: RwLock<RouterState>,
}

impl SessionInteractionRouter {
    /// 构造：借用 session 级交互门（跨轮共享，不新建第二把）。
    pub fn new(session_id: impl Into<String>, gate: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self {
            session_id: session_id.into(),
            gate,
            timeout: crate::broker::ask_user_timeout(),
            state: RwLock::new(RouterState::default()),
        }
    }

    /// 注册（或刷新）一条在线连接 endpoint。
    ///
    /// 幂等：同一 `connection_id` 重复注册保留**首次**纪元（重附着 / 纪元自增
    /// 属阶段 4，见 [`SessionInteractionEndpoint`] 文档）。表满时返回
    /// [`EndpointTableFull`]，调用方必须显式拒绝该请求。
    pub fn register_connection(
        &self,
        connection_id: &str,
        transport: Arc<dyn AcpTransport>,
    ) -> Result<ConnectionEpoch, EndpointTableFull> {
        let mut state = self.state.write();
        if let Some(existing) = state.endpoints.get(connection_id) {
            let epoch = existing.connection_epoch();
            state.endpoints.insert(
                connection_id.to_string(),
                SessionInteractionEndpoint::new(connection_id, epoch, transport),
            );
            return Ok(epoch);
        }
        if state.endpoints.len() >= MAX_ONLINE_ENDPOINTS {
            return Err(EndpointTableFull);
        }
        let epoch = ConnectionEpoch::initial();
        state.endpoints.insert(
            connection_id.to_string(),
            SessionInteractionEndpoint::new(connection_id, epoch, transport),
        );
        Ok(epoch)
    }

    /// 连接关闭 / 退出：置该连接 endpoint 离线、把它承载的全部 View 附件置为
    /// **detach**（身份与纪元保留在有界重挂接窗口内），并在它是当前模型轮
    /// endpoint 时清除模型轮。
    ///
    /// **不撤销记录、不取消**任何已 dispatch 的调用（裁决 D6：连接关闭 = retire
    /// endpoint + 附件 detach；record 的终态撤销只在 `apps/close` / session close /
    /// incarnation 换代 / 保留到期发生）。返回本次被 detach 的附件身份，供调用方
    /// 留痕（relay 侧同步撤交付资格 / 测试断言）。
    pub fn retire_connection(&self, connection_id: &str) -> Vec<AttachmentId> {
        let now = peri_time::monotonic_now();
        let mut state = self.state.write();
        state.expire_detached(now);
        if let Some(endpoint) = state.endpoints.remove(connection_id) {
            endpoint.retire();
        }
        let detached: Vec<AttachmentId> = state
            .attachments
            .values_mut()
            .filter(|attachment| attachment.connection_id() == Some(connection_id))
            .map(|attachment| {
                attachment.state = AttachmentState::Detached {
                    since: now,
                    reattach_deadline: now + Duration::from_millis(DETACH_REATTACH_WINDOW_MS),
                };
                attachment.attachment_id.clone()
            })
            .collect();
        if matches!(state.model_turn.as_ref(), Some((id, _)) if id == connection_id) {
            state.model_turn = None;
        }
        detached
    }

    /// 发布当前模型轮 endpoint：按 prompt 正在使用的 transport 定位已注册连接。
    ///
    /// 未注册的连接 ⇒ `None`（该轮模型审批将得到 `NoOnlineEndpoint`，不静默
    /// 改投别的连接）。
    pub fn publish_model_turn_for_transport(
        &self,
        transport: &Arc<dyn AcpTransport>,
    ) -> Option<ModelTurnToken> {
        let mut state = self.state.write();
        let connection_id = state
            .endpoints
            .values()
            .find(|endpoint| endpoint.is_online() && endpoint.is_transport(transport))
            .map(|endpoint| endpoint.connection_id.clone())?;
        state.next_token = state.next_token.wrapping_add(1);
        let token = ModelTurnToken(state.next_token);
        state.model_turn = Some((connection_id, token));
        Some(token)
    }

    /// 发布当前模型轮 endpoint 并返回**清除守卫**（drop 即清除该轮 endpoint）。
    ///
    /// prompt 装配点用本入口，保证「轮末清除」在正常返回与提前返回（缺 frozen、
    /// 会话不存在等错误路径）下都成立；发布失败（连接未注册 / 已离线）⇒ `None`
    /// （该轮模型审批按无在线 endpoint 处理，不静默改投别的连接）。
    pub fn publish_model_turn(
        self: &Arc<Self>,
        transport: &Arc<dyn AcpTransport>,
    ) -> Option<ModelTurnGuard> {
        let token = self.publish_model_turn_for_transport(transport)?;
        Some(ModelTurnGuard {
            router: Arc::clone(self),
            token,
        })
    }

    /// 清除当前模型轮 endpoint：只在仍是本次发布者时生效（轮末 / prompt 结束）。
    pub fn clear_model_turn(&self, token: ModelTurnToken) {
        let mut state = self.state.write();
        if matches!(state.model_turn.as_ref(), Some((_, current)) if *current == token) {
            state.model_turn = None;
        }
    }

    /// 附着一条 View 附件路由（`apps/attach` 首附 / 旧 `peri/mcp/open` 别名）：
    /// namespace 与纪元（`initial`）由本层分配并返回。
    ///
    /// 承载连接必须是在线 endpoint；表满 / 同 ID 已存在 ⇒ 显式拒绝
    /// （见 [`AttachRejection`]，不淘汰在线附件、不静默复用身份）。
    pub fn attach_view(
        &self,
        request: ViewAttachRequest,
    ) -> Result<SessionAttachment, AttachRejection> {
        self.state
            .write()
            .attach_view(request, peri_time::monotonic_now())
    }

    /// 该连接是否为本会话的**在线 endpoint**（`apps/*` 的边界校验：仅知 sessionId
    /// 不能通过；连接 retire 后立即为假）。
    pub fn is_online_endpoint(&self, connection_id: &str) -> bool {
        self.state
            .read()
            .endpoints
            .get(connection_id)
            .is_some_and(SessionInteractionEndpoint::is_online)
    }

    /// 某个 App 名下的全部附件（`apps/close` 需要在终态撤销**之前**取到
    /// namespace 权威值：撤销后无法反查）。
    pub fn attachments_for_app(&self, app_id: &str) -> Vec<SessionAttachment> {
        self.state
            .read()
            .attachments
            .values()
            .filter(|attachment| attachment.app_id == app_id)
            .cloned()
            .collect()
    }
}

/// 路由面（`apps/attach` 重挂接 / `apps/detach` / `apps/close` / 查询与到期清理）。
///
/// 生产调用者是 `SessionAppsFacade`（S6 接线后已就位），测试消费者是
/// `interaction_router_test.rs`（真实 router，非桩）。
impl SessionInteractionRouter {
    /// 重挂接同一 View（detach 窗口内）：**纪元递增**，旧纪元的批准 / 请求随即失效。
    ///
    /// 承载连接必须是在线 endpoint；附件未知（从未附着 / 窗口已到期）⇒
    /// [`AttachRejection::UnknownAttachment`]（调用方应改用 [`Self::attach_view`]）。
    pub fn reattach_view(
        &self,
        attachment_id: &AttachmentId,
        connection_id: &str,
    ) -> Result<SessionAttachment, AttachRejection> {
        self.state
            .write()
            .reattach_view(attachment_id, connection_id, peri_time::monotonic_now())
    }

    /// detach 一个附件（页面卸载 / 连接退休 / App 关闭）：幂等，**不撤销 record**。
    pub fn detach(
        &self,
        attachment_id: &AttachmentId,
        reason: AttachmentDetachReason,
    ) -> DetachVerdict {
        let now = peri_time::monotonic_now();
        let mut state = self.state.write();
        state.expire_detached(now);
        let Some(attachment) = state.attachments.get_mut(attachment_id) else {
            return DetachVerdict::UnknownAttachment;
        };
        let verdict = match attachment.state {
            AttachmentState::Attached { .. } => {
                attachment.state = AttachmentState::Detached {
                    since: now,
                    reattach_deadline: now + Duration::from_millis(DETACH_REATTACH_WINDOW_MS),
                };
                DetachVerdict::Detached
            }
            AttachmentState::Detached { .. } => DetachVerdict::AlreadyDetached,
        };
        tracing::debug!(
            attachment_id = %attachment_id,
            app_id = %attachment.app_id,
            ?reason,
            ?verdict,
            "session attachment detach"
        );
        verdict
    }

    /// `apps/close`：结束该 App 的**全部** View 附件（终态，不留重挂接窗口）。
    ///
    /// 返回被移除的附件身份（调用方据此同步撤销 relay 侧交付资格）。不取消任何
    /// 已 dispatch 的调用（裁决 D6 / 设计 §5）。
    pub fn close_app_attachments(&self, app_id: &str) -> Vec<AttachmentId> {
        let mut state = self.state.write();
        let removed: Vec<AttachmentId> = state
            .attachments
            .values()
            .filter(|attachment| attachment.app_id == app_id)
            .map(|attachment| attachment.attachment_id.clone())
            .collect();
        for attachment_id in &removed {
            state.attachments.remove(attachment_id);
        }
        removed
    }

    /// 按附件身份查路由表（facade / 测试观察面）。
    ///
    /// 顺带做惰性到期清理：已过重挂接窗口的 detach 条目在此**不再可见**。
    pub fn lookup(&self, attachment_id: &AttachmentId) -> Option<SessionAttachment> {
        let now = peri_time::monotonic_now();
        let mut state = self.state.write();
        state.expire_detached(now);
        state.attachments.get(attachment_id).cloned()
    }

    /// 清理已到期的 detach 条目（幂等；返回本次清理条数）。
    ///
    /// 到期判定本身可单测（`tokio::time::pause` 探针）；后台定时清理留给阶段 5。
    pub fn expire_detached(&self) -> usize {
        let now = peri_time::monotonic_now();
        self.state.write().expire_detached(now)
    }
}

impl SessionInteractionRouter {
    /// 在线 endpoint 数量（测试观察面：路由表有界语义）。
    #[cfg(test)]
    pub(crate) fn online_endpoint_count(&self) -> usize {
        self.state
            .read()
            .endpoints
            .values()
            .filter(|endpoint| endpoint.is_online())
            .count()
    }

    /// 当前模型轮 endpoint 的连接身份（测试观察面）。
    #[cfg(test)]
    pub(crate) fn model_turn_connection(&self) -> Option<String> {
        self.state
            .read()
            .model_turn
            .as_ref()
            .map(|(id, _)| id.clone())
    }

    /// 归属 → 在线 endpoint；失败给出与事实相符的 [`UnansweredCause`]。
    fn route(
        &self,
        owner: &TrustedInteractionOwner,
    ) -> Result<Arc<dyn AcpTransport>, UnansweredCause> {
        let state = self.state.read();
        let resolved: Option<&SessionInteractionEndpoint> = match owner {
            TrustedInteractionOwner::Model { session_id, .. } => {
                if session_id != &self.session_id {
                    // 归属的 session 与路由不符：不投递（fail-closed）。
                    tracing::warn!(
                        router_session = %self.session_id,
                        owner_session = %session_id,
                        "model interaction attribution does not belong to this session router"
                    );
                    None
                } else {
                    state
                        .model_turn
                        .as_ref()
                        .and_then(|(connection_id, _)| state.endpoints.get(connection_id))
                }
            }
            TrustedInteractionOwner::App {
                attachment_id,
                connection_epoch,
                ..
            } => {
                let Some(attachment) = state.attachments.get(attachment_id) else {
                    tracing::info!(
                        attachment = %attachment_id,
                        "no attachment route for app interaction"
                    );
                    return Err(state.unmatched_cause());
                };
                if attachment.epoch != *connection_epoch {
                    // 旧纪元迟到的批准 / 提问：丢弃（不投递到当前连接）。
                    tracing::info!(
                        attachment = %attachment_id,
                        route_epoch = attachment.epoch.get(),
                        owner_epoch = connection_epoch.get(),
                        "dropping interaction from a stale attachment epoch"
                    );
                    return Err(state.unmatched_cause());
                }
                match attachment.connection_id() {
                    Some(connection_id) => state.endpoints.get(connection_id),
                    // 附件与纪元都成立，缺的**只有**投递资格：这是 detach 事实。
                    None => {
                        tracing::info!(
                            attachment = %attachment_id,
                            "app interaction targets a detached view attachment"
                        );
                        return Err(state.view_detached_cause());
                    }
                }
            }
            TrustedInteractionOwner::Host { .. } => match host_request_connection() {
                Some(connection_id) => state.endpoints.get(&connection_id),
                // 调度 / 工作流等 Host 入口没有请求作用域连接：仅当本会话
                // 恰好一个在线 endpoint 时按唯一性投递（多端点时 fail-closed，
                // 不猜归属）。
                None => {
                    let mut online = state
                        .endpoints
                        .values()
                        .filter(|endpoint| endpoint.is_online());
                    let Some(first) = online.next() else {
                        return Err(state.unmatched_cause());
                    };
                    if online.next().is_some() {
                        tracing::warn!(
                            session_id = %self.session_id,
                            "host interaction without request scope: multiple online endpoints, refusing to guess"
                        );
                        return Err(state.unmatched_cause());
                    }
                    Some(first)
                }
            },
        };
        let Some(endpoint) = resolved.filter(|endpoint| endpoint.is_online()) else {
            return Err(state.unmatched_cause());
        };
        Ok(Arc::clone(endpoint.transport()))
    }
}

impl RouterState {
    /// 本端**一个在线 endpoint 都没有** ⇒ `NoOnlineEndpoint`；否则在线 endpoint
    /// **存在**，只是没有一个符合本次归属 ⇒ `OwnerUnmatched`（中 2 的诚实性要求：
    /// 不得让「没有任何在线 endpoint」承载「有 endpoint 但不匹配」的事实）。
    fn unmatched_cause(&self) -> UnansweredCause {
        if self
            .endpoints
            .values()
            .any(SessionInteractionEndpoint::is_online)
        {
            UnansweredCause::OwnerUnmatched
        } else {
            UnansweredCause::NoOnlineEndpoint
        }
    }

    /// 附件已 detach 的原因：本端仍存在在线 endpoint 时是**独立事实**
    /// （`AttachmentDetached`）；一个在线 endpoint 都没有时，`NoOnlineEndpoint`
    /// 本身已完全属实，不另造更弱的说法。
    fn view_detached_cause(&self) -> UnansweredCause {
        let cause = self.unmatched_cause();
        if cause == UnansweredCause::NoOnlineEndpoint {
            return cause;
        }
        UnansweredCause::AttachmentDetached
    }
}

#[async_trait]
impl UserInteractionBroker for SessionInteractionRouter {
    async fn request(
        &self,
        owner: TrustedInteractionOwner,
        context: InteractionContext,
    ) -> InteractionResponse {
        let transport = match self.route(&owner) {
            Ok(transport) => transport,
            Err(cause) => {
                // 本端事实：无法把这次交互投递给任何 endpoint ⇒ 不可能是用户拒绝，
                // 也绝不自动批准（审批侧按「未获批准」处理）。原因按分支如实给出
                // （`NoOnlineEndpoint` / `OwnerUnmatched` / `AttachmentDetached`）。
                tracing::info!(
                    session_id = %self.session_id,
                    owner = %owner,
                    owner_kind = owner.kind(),
                    ?cause,
                    reason = cause.reason_text(),
                    "interaction left unanswered"
                );
                return InteractionResponse::Unanswered { cause };
            }
        };
        // 转发复用既有 ACP broker：审批 / 提问编解码、超时与错误映射单一权威；
        // 门是同一把 session 门（本 router 借用，不重建）。
        let broker = AcpTransportBroker::new(
            Arc::new(AcpRequestBridge(transport)),
            self.session_id.clone().into(),
            Arc::clone(&self.gate),
        )
        .with_timeout(self.timeout);
        broker.request(owner, context).await
    }
}

tokio::task_local! {
    /// 请求作用域注入的 Host 请求连接（`peri/mcp/invoke` 的审批回本连接）。
    static HOST_REQUEST_CONNECTION: String;
}

/// 在请求作用域内绑定「Host 请求所在连接」。
///
/// 由拥有连接事实的调用点（`host/server_loop.rs` 的 `peri/mcp/invoke` 分支）
/// 包住处理逻辑；未绑定的 Host 请求按 [`SessionInteractionRouter::route`] 的
/// 唯一 endpoint 规则处理，或按「无在线 endpoint / 归属不符」显式未投递。
pub async fn scope_host_request_connection<T>(
    connection_id: String,
    future: impl Future<Output = T>,
) -> T {
    HOST_REQUEST_CONNECTION.scope(connection_id, future).await
}

fn host_request_connection() -> Option<String> {
    HOST_REQUEST_CONNECTION.try_with(String::clone).ok()
}

#[cfg(test)]
#[path = "interaction_router_test.rs"]
mod tests;
