use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps_facade::{
    AppQualification, AttachmentBinding, AttachmentSummary, ClientViewId, ModelAssociation,
    RequestNamespaceId, RequestSeq, APP_RETENTION_MS, MAX_APPS_PER_SESSION, MAX_APP_RECORDS,
    MAX_ATTACHMENTS_PER_APP,
};
use peri_acp_types::tool_invocation::{InvocationId, ToolCallId};
use peri_acp_types::tool_outcome::RawToolResult;
use serde_json::{Map, Value};

use super::session_invocation::{AppDeliveryToken, AttachmentDeliveryToken};

pub const MCP_APPS_VERSION: &str = "2026-01-26";
pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";
pub const MCP_UI_EXTENSION: &str = "io.modelcontextprotocol/ui";

/// 附件路由：一个 View 的**连接归属 + 投递资格 + 身份事实**。
///
/// 纪元（`connection_epoch`）取**调用方值**：重挂接时由 router 递增，本层不自行
/// 铸造纪元（旧纪元迟到请求必须被准入层丢弃）。
pub struct AttachmentRoute {
    /// 承载 App 的连接身份（ACP connection）。
    pub connection_id: String,
    /// 连接附件身份。
    pub attachment_id: AttachmentId,
    /// 附件请求域（序号推进权威在 ledger，本层只携带身份）。
    pub namespace: RequestNamespaceId,
    /// 该附件的连接纪元（重连/重新附着时递增；旧纪元迟到请求必须被丢弃）。
    pub connection_epoch: ConnectionEpoch,
    /// 页面身份（只在已认证的会话/客户端作用域内定位）。
    pub client_view_id: ClientViewId,
    /// 附件层交付资格：`detach` / 连接关闭时撤销（不影响已提交调用）。
    ///
    /// 令牌**不实现 `Clone`**：类型上阻止交付资格被随意复制传阅
    ///（路由克隆时共享同一令牌，见 [`Self::clone`]）。
    pub delivery: Arc<AttachmentDeliveryToken>,
}

impl Clone for AttachmentRoute {
    fn clone(&self) -> Self {
        Self {
            connection_id: self.connection_id.clone(),
            attachment_id: self.attachment_id.clone(),
            namespace: self.namespace.clone(),
            connection_epoch: self.connection_epoch,
            client_view_id: self.client_view_id.clone(),
            delivery: Arc::clone(&self.delivery),
        }
    }
}

impl std::fmt::Debug for AttachmentRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachmentRoute")
            .field("connection_id", &self.connection_id)
            .field("attachment_id", &self.attachment_id)
            .field("namespace", &self.namespace)
            .field("connection_epoch", &self.connection_epoch)
            .finish_non_exhaustive()
    }
}

impl AttachmentRoute {
    /// 构造附件路由：纪元 / namespace 由**调用方**给出（router 是纪元权威）。
    pub fn new(
        connection_id: impl Into<String>,
        attachment_id: AttachmentId,
        namespace: RequestNamespaceId,
        connection_epoch: ConnectionEpoch,
        client_view_id: ClientViewId,
    ) -> Self {
        Self {
            connection_id: connection_id.into(),
            attachment_id,
            namespace,
            connection_epoch,
            client_view_id,
            delivery: Arc::new(AttachmentDeliveryToken::new()),
        }
    }

    /// wire 绑定值：`next_seq` 由**调用方**（ledger 的当前推进值）传入，
    /// 本层不保存也不推进序号（单一权威）。
    pub fn binding(&self, next_seq: RequestSeq) -> AttachmentBinding {
        AttachmentBinding {
            attachment_id: self.attachment_id.clone(),
            namespace: self.namespace.clone(),
            connection_epoch: self.connection_epoch,
            next_seq,
        }
    }

    /// descriptor 用的摘要（不含序号）。
    pub fn summary(&self) -> AttachmentSummary {
        AttachmentSummary {
            attachment_id: self.attachment_id.clone(),
            namespace: self.namespace.clone(),
            connection_epoch: self.connection_epoch,
            connection_id: Some(self.connection_id.clone()),
            client_view_id: self.client_view_id.clone(),
        }
    }
}

/// record 的生命周期状态。撤销是**终态**：不得回到 `Attached`。
///
/// `Attached` 表示「已附着过」（当前可能持有 0..n 个附件）：detach 只撤销该附件的
/// 投递资格，record 仍存活于有界重挂接窗口内（裁决 D6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// 已签发、尚未附着任何连接。
    Pending,
    /// 已附着过连接：可受理 App 回调（是否有当前附件见 [`McpAppRecord::is_attached`]）。
    Attached,
    /// 已撤销：准入一律拒绝（零 wire 调用）。
    Revoked,
}

/// entitlement（授权凭据）强制执行状态（裁决 R4）。
///
/// 承载的是**这条 record 自身的凭据事实**，不是部署级强制状态：record 在模型签发点
/// 创建，早于任何 App 回调，构造入参里也没有授权项。4b 批 F 起，ACP facade 在**每次
/// 调用**上按能力消费 `SessionAuthorizationPort`（未注入端口 ⇒ 一律拒绝），因此
/// 「本字段 = `Unenforced`」只表示这条 record 没有承载授权结论，**不**表示部署侧没有
/// 强制（`implementation-log.md` 的 4b 批 F 节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementState {
    /// record 层未承载授权结论：授权是每次请求的事实（caller × session × capability），
    /// 由 facade 的门在调用点判定。
    Unenforced,
}

/// 签发一条 App record 所需的**全部事实**（构造点 = 实例化调用的执行处）。
pub struct McpAppRecordSpec {
    pub owner_session_id: String,
    pub server_id: String,
    pub server_generation: u64,
    pub resource_uri: String,
    pub instantiating_tool: String,
    /// 实例化执行的 canonical 身份。
    pub invocation_id: InvocationId,
    /// 单次消费的实例化令牌（`peri/mcp/open` 用它换取 App 实例）。
    pub invocation_token: String,
    /// 模型关联（`invocation_id` 与 [`Self::invocation_id`] 同源，单次构造）。
    pub tool_call_id: Option<ToolCallId>,
    /// 签发时的会话运行时世代。
    ///
    /// 签发点从注册表读取**已登记**的会话世代；本会话未登记世代时**不签发**
    /// （签发点显式告警，而不是用一个假值放行）——登记责任在会话环境装配点。
    pub incarnation: u64,
    /// App 资格（可交互白名单 / 仅可显示）。
    pub qualification: AppQualification,
    /// 实际（审批后 effective）输入参数。
    pub effective_input: Map<String, Value>,
    /// 完整初始 raw 结果（含 `isError:true` 的结果事实）。
    pub initial_result: Arc<RawToolResult>,
}

/// App 实例的**记录**（取代 `McpAppSelectionLease` 时代的 `McpAppBindingLease`）。
///
/// 纯数据 + 撤销资格 + App 层交付令牌：**不持有** dispatcher、取消令牌
///（调用期事实）、`turn_generation`、catalog 或 `EventBus`（设计 §4 删除清单 3）。
/// `registry → record` 因此是终点（无出边），不再与轮次对象成环。
pub struct McpAppRecord {
    /// 归属会话。
    pub owner_session_id: String,
    /// record 身份（= App 侧 `appId` / 旧 `appSessionId`）。
    pub record_id: String,
    /// 实例化执行的 canonical 身份。
    pub invocation_id: InvocationId,
    pub server_id: String,
    pub server_generation: u64,
    pub resource_uri: String,
    pub instantiating_tool: String,
    /// 单次消费的实例化令牌（`peri/mcp/open`）。
    pub invocation_token: String,
    /// 签发时的会话运行时世代（附着时据此确认记录属当前 incarnation）。
    pub incarnation: u64,
    /// App 资格：可回调（有界白名单）或仅可显示（零 callback 资格，裁决 D5）。
    pub qualification: AppQualification,
    /// 实际（审批后 effective）输入参数：snapshot 的初始输入真相。
    pub effective_input: Map<String, Value>,
    /// entitlement 事实（裁决 R4）。
    pub entitlement: EntitlementState,
    state: Mutex<RecordState>,
    /// 当前附件集合（多 tab / 多 View）；上限 `MAX_ATTACHMENTS_PER_APP`。
    attachments: Mutex<Vec<Arc<AttachmentRoute>>>,
    /// 「本 record 是否曾被附着过」——**有界、无权威**的一次性记忆位。
    ///
    /// 只用于 `apps/attach` 的 `reattached` 事实：detach 会摘掉交付集合里的条目，
    /// 仅看当前集合会把「曾有附件、全部 detach 后新 View 首附」误报成首附。
    /// 它不参与准入、撤销或任何判决（权威仍是附件集合与纪元）。
    ever_attached: AtomicBool,
    delivery: AppDeliveryToken,
    initial_result: Arc<RawToolResult>,
    model: ModelAssociation,
    expires_at: Instant,
}

impl McpAppRecord {
    /// 由**一次**实例化调用的执行事实构造记录（身份字段与模型关联同源构造）。
    pub fn new(spec: McpAppRecordSpec) -> Self {
        Self {
            owner_session_id: spec.owner_session_id,
            record_id: uuid::Uuid::now_v7().to_string(),
            model: ModelAssociation {
                invocation_id: spec.invocation_id.clone(),
                tool_call_id: spec.tool_call_id,
            },
            invocation_id: spec.invocation_id,
            server_id: spec.server_id,
            server_generation: spec.server_generation,
            resource_uri: spec.resource_uri,
            instantiating_tool: spec.instantiating_tool,
            invocation_token: spec.invocation_token,
            incarnation: spec.incarnation,
            qualification: spec.qualification,
            effective_input: spec.effective_input,
            entitlement: EntitlementState::Unenforced,
            state: Mutex::new(RecordState::Pending),
            attachments: Mutex::new(Vec::new()),
            ever_attached: AtomicBool::new(false),
            delivery: AppDeliveryToken::new(),
            initial_result: spec.initial_result,
            expires_at: peri_time::monotonic_now() + Duration::from_millis(APP_RETENTION_MS),
        }
    }

    pub fn state(&self) -> RecordState {
        *self.state.lock()
    }

    /// 当前附件集合快照（路由克隆共享同一交付令牌）。
    pub fn attachments(&self) -> Vec<Arc<AttachmentRoute>> {
        self.attachments.lock().clone()
    }

    /// 按附件身份取路由。
    ///
    /// 准入（`bind_app_target`）与投递（relay）都按**附件身份**在集合中定位：
    /// 多附件 / 多 View 下没有「当前唯一附件」这一事实，本层也不铸造它。
    pub fn attachment_for(&self, attachment_id: &AttachmentId) -> Option<Arc<AttachmentRoute>> {
        self.attachments
            .lock()
            .iter()
            .find(|route| &route.attachment_id == attachment_id)
            .cloned()
    }

    /// 本 record 是否曾被附着过（`reattached` 判据的一部分；无权威，只有界记忆）。
    pub fn ever_attached(&self) -> bool {
        self.ever_attached.load(Ordering::Relaxed)
    }

    /// 附件摘要（descriptor 用；detach 后连接字段缺失）。
    pub fn attachment_summaries(&self) -> Vec<AttachmentSummary> {
        self.attachments
            .lock()
            .iter()
            .map(|route| route.summary())
            .collect()
    }

    /// 模型关联（初始展示事实 ↔ 模型卡片）。
    pub fn model(&self) -> &ModelAssociation {
        &self.model
    }

    /// 完整初始 raw 结果（`Arc` 共享：多 tab attach 不复制，设计 §9.1）。
    pub fn initial_result(&self) -> &Arc<RawToolResult> {
        &self.initial_result
    }

    /// 保留期剩余（到期即终态撤销）。
    pub fn retention_remaining(&self) -> Duration {
        self.expires_at
            .saturating_duration_since(peri_time::monotonic_now())
    }

    /// App 层交付/撤销令牌（`apps/close` 时撤销）。
    pub fn delivery(&self) -> &AppDeliveryToken {
        &self.delivery
    }

    /// 记录仍存活：未过期且未撤销（待附着或已附着）；注册表的有界清理用它。
    pub fn is_live(&self) -> bool {
        self.is_live_at(peri_time::monotonic_now())
    }

    /// 在**给定时刻**是否存活（到期判定与时钟解耦的唯一形状）。
    ///
    /// 生产路径一律走 [`Self::is_live`]；本方法存在是为了让「等于保留期 ⇒ 仍存活 /
    /// 超过保留期 ⇒ 已不存活」这条边界可断言（`peri_time` 无测试时钟，`Instant` 是
    /// 单调时钟、测试内推不动）。
    pub(crate) fn is_live_at(&self, now: Instant) -> bool {
        now < self.expires_at && self.state() != RecordState::Revoked
    }

    /// 未过期、有当前附件、未撤销：可以受理新的调用。
    pub fn is_attached(&self) -> bool {
        self.is_live() && !self.attachments.lock().is_empty() && !self.delivery.is_revoked()
    }

    /// 附着附件（首个附件把 `Pending → Attached`）；已撤销的记录不得附着，
    /// 附件数上限见 `MAX_ATTACHMENTS_PER_APP`（4a 保守值）。
    pub fn attach(&self, route: Arc<AttachmentRoute>) -> bool {
        let mut state = self.state.lock();
        if *state == RecordState::Revoked {
            return false;
        }
        let mut attachments = self.attachments.lock();
        if attachments.len() as u64 >= MAX_ATTACHMENTS_PER_APP {
            return false;
        }
        if attachments
            .iter()
            .any(|existing| existing.attachment_id == route.attachment_id)
        {
            return false;
        }
        attachments.push(route);
        *state = RecordState::Attached;
        self.ever_attached.store(true, Ordering::Relaxed);
        true
    }

    /// 重挂接：同一附件身份换新纪元（**纪元由调用方给出**）。
    ///
    /// 旧路由的交付令牌立即撤销（旧连接不得再收数据 / 提交请求）；返回是否发生了
    /// 替换。附件不在集合中 ⇒ 不替换（调用方应改用 [`Self::attach`]）。
    pub fn rebind_attachment(&self, route: Arc<AttachmentRoute>) -> bool {
        if self.state() == RecordState::Revoked {
            return false;
        }
        let mut attachments = self.attachments.lock();
        let Some(slot) = attachments
            .iter_mut()
            .find(|existing| existing.attachment_id == route.attachment_id)
        else {
            return false;
        };
        let previous = std::mem::replace(slot, route);
        drop(attachments);
        previous.delivery.revoke();
        self.ever_attached.store(true, Ordering::Relaxed);
        true
    }

    /// 撤销一个附件的投递资格；**不撤销 record**（有界重挂接窗口内可再挂）。
    ///
    /// 返回是否发生了撤销（幂等）。
    pub fn detach_attachment(&self, attachment_id: &AttachmentId) -> bool {
        let mut attachments = self.attachments.lock();
        let Some(index) = attachments
            .iter()
            .position(|route| &route.attachment_id == attachment_id)
        else {
            return false;
        };
        let route = attachments.remove(index);
        drop(attachments);
        route.delivery.revoke();
        true
    }

    /// 撤销（终态）：置 `Revoked` 并撤销本记录**全部**附件的交付资格。
    ///
    /// **不取消**已提交的执行（设计 §2：关闭 App 不取消同轮执行，由 invocation
    /// owner 收尾）。
    pub fn revoke(&self) -> bool {
        let mut state = self.state.lock();
        if *state == RecordState::Revoked {
            return false;
        }
        *state = RecordState::Revoked;
        drop(state);
        self.delivery.revoke();
        let attachments = std::mem::take(&mut *self.attachments.lock());
        for route in attachments {
            route.delivery.revoke();
        }
        true
    }

    /// App 面工具名 → effective 工具名（准入白名单）。
    ///
    /// `DisplayOnly` 记录没有任何可回调工具 ⇒ 恒 `None`（零 callback 资格）。
    pub fn allowed_effective_name(&self, wire_name: &str) -> Option<&String> {
        self.qualification.allowed_effective_name(wire_name)
    }

    /// effective 工具名是否仍在本记录的资格白名单内（提交复核用）。
    pub fn allows_effective_name(&self, effective_name: &str) -> bool {
        self.qualification
            .allowed_tools()
            .is_some_and(|allowed| allowed.values().any(|value| value == effective_name))
    }
}

/// 签发被拒的原因（显式承载，调用点必须记录，不静默丢弃）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueRejection {
    /// 本会话已登记世代，而记录携带的世代与之不符（旧环境的迟到签发）。
    IncarnationMismatch {
        /// 记录携带的世代。
        bound: u64,
        /// 注册表当前登记的世代。
        current: u64,
    },
    /// 本会话存活记录数已达 `MAX_APPS_PER_SESSION`（单会话**契约**上限）。
    ///
    /// 拒绝本次签发、保留既有存活记录：不淘汰（淘汰会让用户正在看的 App 静默
    /// 消失），与 `MAX_ATTACHMENTS_PER_SESSION` 的「表满显式拒绝」同策。
    SessionCapacityExceeded {
        /// 单会话上限（= `MAX_APPS_PER_SESSION`）。
        limit: u64,
    },
}

impl std::fmt::Display for IssueRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncarnationMismatch { bound, current } => write!(
                f,
                "session incarnation mismatch: record bound to {bound}, current {current}"
            ),
            Self::SessionCapacityExceeded { limit } => write!(
                f,
                "session app record table is full ({limit}); today's record is not issued and \
                 existing live records are kept"
            ),
        }
    }
}

/// App 实例记录注册表（纯数据；**不暴露** `cancel()`）。
///
/// 撤销面只有 [`Self::revoke_app`] / [`Self::revoke_connection`] /
/// [`Self::revoke_session`]：它们只置状态与撤销交付资格，不中断执行。
///
/// 另持有**会话运行时世代登记**（`session_id → incarnation`）：签发点据此盖章，
/// 装配点（会话环境）负责登记（未登记 ⇒ 签发点不签发并显式告警）。
#[derive(Default)]
pub struct McpAppBindingRegistry {
    /// 已签发记录，按 (server_id, instantiating_tool) 分桶（同一槽位可有多条
    /// 待消费记录）。
    records: Mutex<HashMap<(String, String), Vec<Arc<McpAppRecord>>>>,
    /// `record_id` → record（App 回调与按身份撤销的唯一入口）。
    by_id: Mutex<HashMap<String, Arc<McpAppRecord>>>,
    /// 会话运行时世代（session incarnation）登记；会话关闭即清除（有界）。
    incarnations: Mutex<HashMap<String, u64>>,
}

impl McpAppBindingRegistry {
    /// 登记会话运行时世代。**装配点唯一调用者**：会话环境装配（含 restore / load
    /// 的复用路径）——连接与记录的归属是会话，不是某一轮。
    pub fn register_session_incarnation(&self, session_id: &str, incarnation: u64) {
        self.incarnations
            .lock()
            .insert(session_id.to_string(), incarnation);
    }

    /// 当前登记的会话运行时世代（未登记 ⇒ `None`，签发点不得猜测）。
    pub fn session_incarnation(&self, session_id: &str) -> Option<u64> {
        self.incarnations.lock().get(session_id).copied()
    }

    /// 有界清理：过期 / 已撤销条目出表，超出容量淘汰**最旧**记录（并终态撤销）。
    ///
    /// 唯一调用点是 [`Self::issue`] 的**登记之后**（4b 批 H 起；登记前调用会让表长
    /// 可以停在「上限 + 1」，上限就名不副实）。
    ///
    /// 两把锁**顺序获取**（先 `records` 后 `by_id`），与 [`Self::revoke_app`]
    /// 同序，不存在交叉持锁。附件随记录存活：记录出表即撤销其**全部**附件交付
    /// 资格（detach 过的附件已不在集合中，不会残留令牌）。
    fn cleanup(&self) {
        {
            let mut records = self.records.lock();
            records.retain(|_, values| {
                values.retain(|record| record.is_live());
                !values.is_empty()
            });
        }
        let mut evicted: Vec<Arc<McpAppRecord>> = Vec::new();
        {
            let mut by_id = self.by_id.lock();
            by_id.retain(|_, record| record.is_live());
            while by_id.len() > MAX_APP_RECORDS as usize {
                // 淘汰**最旧**：record 身份是 UUID v7（时间有序），按身份取最小即最旧。
                // 不得用 `keys().next()`——`HashMap` 的迭代序不是插入序，那样会随机
                // 淘汰，甚至淘汰掉刚签发的记录。同一毫秒内签发的身份之间顺序随机
                //（尾段是随机位），此时等同「同刻签发者中任选」，可接受。
                let Some(key) = by_id.keys().min().cloned() else {
                    break;
                };
                if let Some(record) = by_id.remove(&key) {
                    evicted.push(record);
                }
            }
        }
        for record in evicted {
            record.revoke();
        }
    }

    /// 本会话**存活**记录数（自己过滤 `is_live()`，不依赖 [`Self::cleanup`] 是否已跑）。
    fn live_records_of_session(&self, session_id: &str) -> usize {
        self.by_id
            .lock()
            .values()
            .filter(|record| record.owner_session_id == session_id && record.is_live())
            .count()
    }

    /// 登记一条新签发的记录。
    ///
    /// 世代一致性是**准入前置**：若本会话已登记世代而记录携带的世代与之不符
    /// （旧环境的迟到签发），拒绝登记——绝不签发世代不符的记录。
    ///
    /// 单会话容量（`MAX_APPS_PER_SESSION`）同样是准入前置：已达上限则拒绝本次签发
    /// 并保留既有存活记录（**不淘汰**——淘汰会让用户正在看的 App 静默消失）。
    /// 跨会话的注册表容量（`MAX_APP_RECORDS`）在登记后由 [`Self::cleanup`] 按最旧淘汰。
    pub fn issue(&self, record: Arc<McpAppRecord>) -> Result<(), IssueRejection> {
        if let Some(current) = self.session_incarnation(&record.owner_session_id) {
            if record.incarnation != current {
                return Err(IssueRejection::IncarnationMismatch {
                    bound: record.incarnation,
                    current,
                });
            }
        }
        if self.live_records_of_session(&record.owner_session_id) >= MAX_APPS_PER_SESSION as usize {
            return Err(IssueRejection::SessionCapacityExceeded {
                limit: MAX_APPS_PER_SESSION,
            });
        }
        self.records
            .lock()
            .entry((record.server_id.clone(), record.instantiating_tool.clone()))
            .or_default()
            .push(Arc::clone(&record));
        self.by_id
            .lock()
            .insert(record.record_id.clone(), Arc::clone(&record));
        // 有界清理**在登记之后**：`cleanup` 的前置调用（旧写法）只把表清理到上限就插入，
        // 于是「表 + 1」在两次签发之间一直存在——上限被算成 `MAX_APP_RECORDS + 1`。
        // 登记后清理才能让「任何时刻的表长 ≤ 上限」成立（刚签发的记录是最新的，
        // 淘汰按最旧优先，不会命中它，除非全部记录落在同一毫秒——见 `cleanup` 注释）。
        self.cleanup();
        Ok(())
    }

    /// 按 record 身份取记录（App 回调 / 撤销）。
    pub fn find(&self, record_id: &str) -> Option<Arc<McpAppRecord>> {
        self.by_id.lock().get(record_id).cloned()
    }

    /// 本会话的全部记录（会话关闭时撤销）。
    pub fn records_for_session(&self, session_id: &str) -> Vec<Arc<McpAppRecord>> {
        self.by_id
            .lock()
            .values()
            .filter(|record| record.owner_session_id == session_id)
            .cloned()
            .collect()
    }

    /// 按附件身份找到持有它的记录（`apps/detach` / 连接 retire 的入口）。
    pub fn record_for_attachment(&self, attachment_id: &AttachmentId) -> Option<Arc<McpAppRecord>> {
        self.by_id
            .lock()
            .values()
            .find(|record| record.attachment_for(attachment_id).is_some())
            .cloned()
    }

    /// 本连接承载附件的全部记录（连接关闭时该连接的附件 detach）。
    pub fn records_for_connection(&self, connection_id: &str) -> Vec<Arc<McpAppRecord>> {
        self.by_id
            .lock()
            .values()
            .filter(|record| {
                record
                    .attachments()
                    .iter()
                    .any(|route| route.connection_id == connection_id)
            })
            .cloned()
            .collect()
    }

    /// 撤销单个 App record；返回是否发生了状态转移。
    ///
    /// 撤销是终态：条目同时从两个索引移除（撤销后的 record 不可再寻址，也不再被
    /// 注册表持有——这正是「无 Arc 回环」的可观察形式）。
    pub fn revoke_app(&self, record_id: &str) -> bool {
        let Some(record) = self.by_id.lock().remove(record_id) else {
            return false;
        };
        {
            let mut records = self.records.lock();
            if let Some(values) =
                records.get_mut(&(record.server_id.clone(), record.instantiating_tool.clone()))
            {
                values.retain(|candidate| candidate.record_id != record.record_id);
            }
            records.retain(|_, values| !values.is_empty());
        }
        record.revoke()
    }

    /// 撤销某连接上的全部 record，返回被撤销记录的附件身份（供 service 同步撤销）。
    pub fn revoke_connection(&self, connection_id: &str) -> Vec<AttachmentId> {
        let mut detached = Vec::new();
        for record in self.records_for_connection(connection_id) {
            let on_connection: Vec<AttachmentId> = record
                .attachments()
                .iter()
                .filter(|route| route.connection_id == connection_id)
                .map(|route| route.attachment_id.clone())
                .collect();
            if self.revoke_app(&record.record_id) {
                detached.extend(on_connection);
            }
        }
        detached
    }

    /// detach 某连接承载的全部附件（**不撤销 record**，裁决 D6）。
    ///
    /// 返回被 detach 的附件身份（供 service 同步撤销交付资格）。**这是连接关闭**
    /// （`apps_relay.rs::close_connection`）**的现行语义**：连接退出只撤该连接的
    /// 投递资格，record 与模型历史保留；record 的终态撤销只在 `apps/close` /
    /// session close / incarnation 换代 / 保留到期发生。
    pub fn detach_connection(&self, connection_id: &str) -> Vec<AttachmentId> {
        let mut detached = Vec::new();
        for record in self.records_for_connection(connection_id) {
            let on_connection: Vec<AttachmentId> = record
                .attachments()
                .iter()
                .filter(|route| route.connection_id == connection_id)
                .map(|route| route.attachment_id.clone())
                .collect();
            for attachment_id in on_connection {
                if record.detach_attachment(&attachment_id) {
                    detached.push(attachment_id);
                }
            }
        }
        detached
    }

    /// 撤销某会话的全部 record（会话关闭），并清除该会话的世代登记。
    pub fn revoke_session(&self, owner_session_id: &str) -> Vec<AttachmentId> {
        self.incarnations.lock().remove(owner_session_id);
        let mut detached = Vec::new();
        for record in self.records_for_session(owner_session_id) {
            let attachments: Vec<AttachmentId> = record
                .attachments()
                .iter()
                .map(|route| route.attachment_id.clone())
                .collect();
            self.revoke_app(&record.record_id);
            detached.extend(attachments);
        }
        detached
    }
}

pub const MCP_APPS_ENV: &str = "PERI_MCP_APPS";

pub fn deployment_profile(apps_enabled: bool) -> McpCapabilityProfile {
    if apps_enabled {
        McpCapabilityProfile::negotiated([MCP_APP_MIME_TYPE])
    } else {
        McpCapabilityProfile::disabled()
    }
}

/// 由 ACP connection 注入的不可变 MCP capability profile。
///
/// 默认值关闭 Apps；只有显式协商出的受支持 MIME 才会进入 profile。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpCapabilityProfile {
    apps_mime_types: BTreeSet<String>,
}

impl McpCapabilityProfile {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn negotiated<'a>(mime_types: impl IntoIterator<Item = &'a str>) -> Self {
        let apps_mime_types = mime_types
            .into_iter()
            .filter(|mime| *mime == MCP_APP_MIME_TYPE)
            .map(str::to_owned)
            .collect();
        Self { apps_mime_types }
    }

    pub fn apps_enabled(&self) -> bool {
        !self.apps_mime_types.is_empty()
    }

    pub fn apps_mime_types(&self) -> impl Iterator<Item = &str> {
        self.apps_mime_types.iter().map(String::as_str)
    }

    pub(crate) fn ui_extension(&self) -> Option<Map<String, Value>> {
        self.apps_enabled().then(|| {
            Map::from_iter([(
                "mimeTypes".to_string(),
                Value::Array(
                    self.apps_mime_types()
                        .map(|mime| Value::String(mime.to_string()))
                        .collect(),
                ),
            )])
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolVisibility {
    pub model: bool,
    pub app: bool,
}

impl ToolVisibility {
    /// 缺失时兼容 model + app；任何 malformed、空数组或未知值均关闭两面。
    pub fn from_tool_meta(meta: Option<&Value>) -> Self {
        let Some(meta) = meta else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(meta) = meta.as_object() else {
            return Self {
                model: false,
                app: false,
            };
        };
        let Some(ui) = meta.get("ui") else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(ui) = ui.as_object() else {
            return Self {
                model: false,
                app: false,
            };
        };
        let Some(visibility) = ui.get("visibility") else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(values) = visibility.as_array() else {
            return Self {
                model: false,
                app: false,
            };
        };
        if values.is_empty()
            || values
                .iter()
                .any(|value| !matches!(value.as_str(), Some("model") | Some("app")))
        {
            return Self {
                model: false,
                app: false,
            };
        }
        Self {
            model: values.iter().any(|value| value == "model"),
            app: values.iter().any(|value| value == "app"),
        }
    }
}

/// 读取并 canonicalize `_meta.ui.resourceUri`；legacy key 仅作兼容输入。
/// canonical 与 legacy 冲突或值类型错误时 fail closed。
pub fn canonical_resource_uri(meta: Option<&Value>) -> Option<String> {
    let meta = meta?.as_object()?;
    let canonical = meta
        .get("ui")
        .and_then(Value::as_object)
        .and_then(|ui| ui.get("resourceUri"));
    let legacy = meta.get("ui/resourceUri");
    match (canonical, legacy) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => value
            .as_str()
            .filter(|uri| uri.starts_with("ui://"))
            .map(str::to_owned),
        (None, None) => None,
    }
}

pub fn tool_visibility(tool: &rmcp::model::Tool) -> ToolVisibility {
    let value = serde_json::to_value(tool).unwrap_or(Value::Null);
    ToolVisibility::from_tool_meta(value.get("_meta"))
}

pub fn tool_resource_uri(tool: &rmcp::model::Tool) -> Option<String> {
    let value = serde_json::to_value(tool).ok()?;
    canonical_resource_uri(value.get("_meta"))
}

pub fn raw_tool(tool: &rmcp::model::Tool) -> RawMcpTool {
    serde_json::to_value(tool).unwrap_or(Value::Null)
}

pub fn raw_resource(resource: &rmcp::model::Resource) -> RawMcpResource {
    serde_json::to_value(resource).unwrap_or(Value::Null)
}

/// Raw MCP payload stays as JSON until the legacy model projection boundary.
pub type RawMcpTool = Value;
pub type RawMcpResource = Value;

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
