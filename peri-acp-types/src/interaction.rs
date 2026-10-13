//! 人机交互契约（自 peri-agent 迁入；`peri-agent::interaction` 保留 re-export）。
//!
//! 统一 HITL（工具审批）与 AskUser（问答）两条路径。
//!
//! 请求带[可信归属](TrustedInteractionOwner)：交互路由按归属选择审批
//! endpoint、归属答复，并在后续批次据此消费 session entitlement（设计 §4.3）。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    identity::AttemptId,
    session::{ExecutionBinding, TurnId},
};

// ─── ApprovalItem ──────────────────────────────────────────────────────────────

/// 工具调用审批项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalItem {
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

// ─── QuestionItem ──────────────────────────────────────────────────────────────

/// 问题选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单个问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

// ─── InteractionContext ────────────────────────────────────────────────────────

/// 人机交互上下文（描述需要用户响应的场景）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionContext {
    /// 工具调用前审批（原 HITL BatchApprovalRequest）
    Approval { items: Vec<ApprovalItem> },
    /// 向用户提问（原 AskUserBatchRequest）
    Questions { requests: Vec<QuestionItem> },
}

// ─── InteractionResponse ───────────────────────────────────────────────────────

/// 单项审批决策（对齐 HitlDecision 四种语义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Approve {
        source: Option<String>,
    },
    Reject {
        reason: String,
        source: Option<String>,
    },
    Edit {
        new_input: serde_json::Value,
    },
    Respond {
        message: String,
    },
}

/// 问题答案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub selected: Vec<String>,
    pub text: Option<String>,
}

/// 交互响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionResponse {
    Decisions(Vec<ApprovalDecision>),
    Answers(Vec<QuestionAnswer>),
    /// 用户明确拒绝了交互（如在 AskUserQuestion 确认弹窗中选择拒绝）
    Rejected,
    /// 无人可以作答：客户端声明自身正常收到提问但无法征询用户（如 `-p` 打印
    /// 模式），或本部署**没有任何在线审批 endpoint**（`cause =
    /// NoOnlineEndpoint`，本端事实）。与 [`InteractionResponse::Rejected`]
    /// （用户拒绝）和空 `Answers`（伪造成功）都不同，工具须如实转述 `cause`
    /// 而不是当作已回答。
    ///
    /// owner 失效、session/prompt 取消、投递失败等生命周期取消不属此变体：它们
    /// 保持既有 cancel 语义，不得伪称客户端声明了原因。审批侧同理：本变体不是
    /// 「用户拒绝」，实现方不得把它投影成自动批准。
    Unanswered {
        cause: UnansweredCause,
    },
}

// ─── UnansweredCause ───────────────────────────────────────────────────────────

/// elicitation 响应的 `_meta` 键：客户端声明「问题无法被作答」的原因。
///
/// 无交互界面的客户端（如 `-p` 打印模式）在取消 elicitation 时携带此键，
/// broker 据此产出 [`InteractionResponse::Unanswered`]；键缺失保持旧兼容，
/// 键存在但取值无法识别按 [`UnansweredCause::Unknown`] 处理。
pub const ELICITATION_UNANSWERED_META_KEY: &str = "peri.elicitationUnanswered";

/// 客户端无法作答的原因（非用户选择）
///
/// 只承载「无人可作答」这一事实的分类，不承载客户端自由文本：工具据此如实
/// 说明，不转述、不猜测声明之外的内容。
///
/// 两个来源共用一个枚举（单一权威，不另造平行枚举）：
/// 客户端在 elicitation 响应里声明（[`UnansweredCause::NonInteractiveClient`] /
/// [`UnansweredCause::Unknown`]），或本端交互路由无法投递（`NoOnlineEndpoint` /
/// `OwnerUnmatched` / `AttachmentDetached`，均由 Peri 自己铸造）。任一来源都只
/// 意味着「没有得到用户答复」，因此不得被当成用户同意。
///
/// 本端铸造的三个变体**互不重叠**，一个变体只承载一种事实（诚实性要求）：
/// 本端完全没有在线 endpoint / 本端有在线 endpoint 但没有一个符合本次归属 /
/// 本次归属的 View 附件已 detach。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnansweredCause {
    /// 客户端没有交互界面（如 `-p` 打印模式）：提问已正常送达，但不可能得到用户回答
    NonInteractiveClient,
    /// 客户端声明了「无人可作答」但取值无法识别（更新的客户端或畸形声明）：
    /// 只知道没有用户可以回答，不得声称具体是哪种客户端。
    Unknown,
    /// 本端没有任何可投递的在线审批 endpoint（设计 §4.3）：请求无法送达任何
    /// 客户端，因此不可能得到用户答复，操作保持**未执行**。
    ///
    /// 本变体的**唯一**含义是「一个在线 endpoint 都没有」；本端**有**在线
    /// endpoint 但没有一个符合本次归属时，必须用 [`UnansweredCause::OwnerUnmatched`]
    /// 而不是本变体（否则 `reason_text()` 会向模型谎称「没有任何在线 endpoint」）。
    ///
    /// 这是**本端事实**，不是客户端声明，也不是用户拒绝：实现方不得据此降级为
    /// 自动批准（尤其不得回落到 `PermissionMiddleware::disabled()`），审批侧须按
    /// 「未获批准」处理。Bypass 仍由原 Permission 权威裁决，不由本原因放行。
    NoOnlineEndpoint,
    /// 本端**存在**在线的审批 endpoint，但没有任何一个符合本次交互的归属
    /// （归属会话与本端不符 / 本轮没有发布模型轮 endpoint / 附件不在本端路由表 /
    /// 附件纪元不符 / 承载连接未注册或已离线 / Host 无请求作用域且多端点歧义）。
    ///
    /// 与 [`UnansweredCause::NoOnlineEndpoint`] 的分工是**唯一**的：本变体成立时
    /// 本端一定有在线 endpoint（只是不匹配），因此 `reason_text()` 不得声称
    /// 「没有任何在线 endpoint」。
    ///
    /// 同样是**本端事实**：不是用户拒绝，不是自动批准，操作保持未执行。
    OwnerUnmatched,
    /// 本次归属的 View 附件已 detach（页面卸载 / 承载连接退休），且有界重挂接
    /// 窗口内尚未重挂接：本端不再持有承载它的 endpoint，交互无法投递。
    ///
    /// 与 [`UnansweredCause::OwnerUnmatched`] 的分工：本变体下附件与纪元都成立，
    /// **只有投递资格**（承载连接）缺失——重挂接即可恢复；不是用户拒绝，也不放行。
    AttachmentDetached,
}

impl UnansweredCause {
    /// 供工具如实转述给模型的说明（不含伪造答案，不指导代替用户授权）。
    pub fn reason_text(self) -> &'static str {
        match self {
            Self::NonInteractiveClient => {
                "客户端是非交互客户端（如 `-p` 打印模式）：提问已正常送达，但该客户端\
                 没有交互界面，没有用户可以作答；这不是用户拒绝或同意，不要把它当作\
                 回答，也不要代替用户完成需要授权的决定"
            }
            Self::Unknown => {
                "客户端声明本次提问无人可作答，但未给出可识别的原因；这不是用户拒绝\
                 或同意，不要把它当作回答，也不要代替用户完成需要授权的决定"
            }
            Self::NoOnlineEndpoint => {
                "当前没有任何在线的审批 endpoint：本次交互无法送达任何客户端，没有\
                 用户可以作答，操作保持未执行；这不是用户拒绝或同意，不要把它当作\
                 回答或授权"
            }
            Self::OwnerUnmatched => {
                "本端有在线的审批 endpoint，但没有任何一个符合本次交互的归属（承载\
                 会话 / 模型轮 / 附件或连接不匹配）：本次交互无法送达任何客户端，没有\
                 用户可以作答，操作保持未执行；这不是用户拒绝或同意，不要把它当作\
                 回答或授权"
            }
            Self::AttachmentDetached => {
                "本次交互所属的 View 附件已 detach（页面卸载或承载连接退出），且有界\
                 窗口内尚未重挂接：本端没有可投递它的 endpoint，没有用户可以作答，\
                 操作保持未执行；这不是用户拒绝或同意，不要把它当作回答或授权"
            }
        }
    }

    /// 从 elicitation 响应的 `_meta` 读取客户端声明的原因。
    ///
    /// 只有键（乃至整份 `_meta`）缺失才返回 `None` —— 旧客户端的裸 cancel 语义；
    /// 键存在但取值无法解析时返回 [`UnansweredCause::Unknown`]：声明仍然成立，
    /// 不得当成未声明而伪造空回答。
    ///
    /// [`UnansweredCause::NoOnlineEndpoint`] / [`UnansweredCause::OwnerUnmatched`] /
    /// [`UnansweredCause::AttachmentDetached`] 由 Peri 自己铸造（[`Self::meta_entry`]
    /// 可把它们编码进 `_meta` 供客户端复用同一词表）；客户端若声明这些原因，语义与
    /// 其它「无人可作答」声明一致：不构成回答，也不构成授权。
    ///
    /// # Examples
    ///
    /// ```
    /// use peri_acp_types::interaction::{ELICITATION_UNANSWERED_META_KEY, UnansweredCause};
    /// use serde_json::{Map, Value};
    ///
    /// let mut meta = Map::new();
    /// assert_eq!(UnansweredCause::from_meta(Some(&meta)), None);
    ///
    /// meta.insert(
    ///     ELICITATION_UNANSWERED_META_KEY.to_string(),
    ///     Value::String("non_interactive_client".to_string()),
    /// );
    /// assert_eq!(
    ///     UnansweredCause::from_meta(Some(&meta)),
    ///     Some(UnansweredCause::NonInteractiveClient)
    /// );
    ///
    /// meta.insert(
    ///     ELICITATION_UNANSWERED_META_KEY.to_string(),
    ///     Value::String("some_future_client".to_string()),
    /// );
    /// assert_eq!(
    ///     UnansweredCause::from_meta(Some(&meta)),
    ///     Some(UnansweredCause::Unknown)
    /// );
    /// ```
    pub fn from_meta(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> Option<Self> {
        let value = meta?.get(ELICITATION_UNANSWERED_META_KEY)?;
        Some(Self::deserialize(value).unwrap_or(Self::Unknown))
    }

    /// 构造 `_meta` 条目，供客户端在 elicitation 响应中声明该原因。
    pub fn meta_entry(self) -> (String, serde_json::Value) {
        (
            ELICITATION_UNANSWERED_META_KEY.to_string(),
            serde_json::to_value(self).expect("unit-variant cause must serialize"),
        )
    }
}

// ─── TrustedInteractionOwner ───────────────────────────────────────────────────

/// 承载一次 App 交互的连接附件身份。
///
/// 阶段 3 落地完整 Attachment（请求序号窗口、交付资格）前的**路由身份**：
/// 只表达「这一次询问来自哪个 Attachment」。与
/// [`crate::tool_invocation::InvocationOrigin::App`] 的 `attachment_id`
/// （同一事实的 canonical 执行来源侧投影）指同一 Attachment，两者不得互相
/// 替代：本类型只出现在交互归属上。
///
/// # Examples
///
/// ```
/// use peri_acp_types::interaction::AttachmentId;
///
/// let id = AttachmentId::new("att-1");
/// assert_eq!(id.as_str(), "att-1");
/// assert_eq!(id.to_string(), "att-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentId(String);

impl AttachmentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AttachmentId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for AttachmentId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for AttachmentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 同一 View 的当前连接纪元：重连时递增，旧连接随即失去提交、审批回答、
/// 取消新请求与接收数据的资格（设计 §4「Attachment」行）。
///
/// 语义与 [`crate::identity::SessionEpoch`] 同族但**不同事实**：本类型标识
/// 某次连接，不是 session incarnation；不得用 session epoch 冒充。
///
/// # Examples
///
/// ```
/// use peri_acp_types::interaction::ConnectionEpoch;
///
/// let first = ConnectionEpoch::initial();
/// assert_eq!(first.get(), 1);
/// // 重连：只增不减，旧纪元不得复用。
/// assert_eq!(first.next().get(), 2);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionEpoch(u64);

impl ConnectionEpoch {
    /// 首次连接的纪元（1；0 保留给「未知/未分配」）。
    pub const fn initial() -> Self {
        Self(1)
    }

    /// 从已知值构造（0 = 未知纪元的显式表达，不表示「首次连接」）。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 递增到下个纪元（重连时调用）。只增不减，旧纪元不得复用。
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// 底层值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Default for ConnectionEpoch {
    fn default() -> Self {
        Self::initial()
    }
}

impl std::fmt::Display for ConnectionEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 一次交互请求的**归属事实**（谁在问）的唯一事实源。
///
/// 与 [`TrustedInteractionOwner`] 的分工：本类型承载**事实**（真实执行事实的
/// 投影），owner 是**路由身份**。二者之间的映射**单一权威**在本类型
/// （[`InteractionAttribution::owner`]）：调用方不得各自拼装 owner，也不得把
/// 归属降级为「session + turn 绑定」这一类可自述的字段组合。
///
/// 三个变体对应三种真实来源（设计 §4.3）：
///
/// - [`Self::ModelTurn`]：真实模型 turn 事实（session 身份 + `ExecutionBinding`），
///   由 `InvocationContext` 从 `TurnContext` 投影；
/// - [`Self::AppAttachment`]：App 宿主经**已附着**的连接附件发起（附件身份 +
///   该附件当前连接纪元）；未附着的 App 不产生归属（`None`），交互因此 fail-closed；
/// - [`Self::HostRequest`]：Host 程序化入口发起，字段与
///   [`crate::tool_invocation::InvocationOrigin::Host`] 同形。
///
/// 缺归属时调用方必须显式拒绝该交互，不得伪造成上述任一归属。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum InteractionAttribution {
    /// 模型 turn 内的交互。
    ModelTurn {
        /// 承载本次 turn 的 session。
        session_id: String,
        /// turn / attempt 绑定（事件纽带身份的 turn 部分）。
        binding: ExecutionBinding,
    },
    /// MCP App 宿主经已附着的连接附件发起的交互。
    AppAttachment {
        /// App 实例身份。
        app_id: String,
        /// 承载本次交互的连接附件。
        attachment_id: AttachmentId,
        /// 该 Attachment 的当前连接纪元（重连后旧纪元迟到交互必须被丢弃）。
        connection_epoch: ConnectionEpoch,
    },
    /// Host 程序化入口发起的交互。
    HostRequest {
        /// 宿主入口命名空间（如 `peri/mcp/invoke`）。
        namespace: String,
        /// 同一 namespace 内单调递增的调用序号。
        seq: u64,
    },
}

impl InteractionAttribution {
    /// 从真实 turn 事实铸造模型归属事实。
    ///
    /// `binding` 必须来自 [`crate::session::TurnContext::execution_binding`] 或其
    /// 等价投影（`InvocationContext`），不得手工拼装。
    pub fn model_turn(session_id: impl Into<String>, binding: ExecutionBinding) -> Self {
        Self::ModelTurn {
            session_id: session_id.into(),
            binding,
        }
    }

    /// 从 Attachment 事实铸造 App 归属事实。
    pub fn app_attachment(
        app_id: impl Into<String>,
        attachment_id: impl Into<AttachmentId>,
        connection_epoch: ConnectionEpoch,
    ) -> Self {
        Self::AppAttachment {
            app_id: app_id.into(),
            attachment_id: attachment_id.into(),
            connection_epoch,
        }
    }

    /// 从 Host 入口事实铸造程序化归属事实。
    pub fn host_request(namespace: impl Into<String>, seq: u64) -> Self {
        Self::HostRequest {
            namespace: namespace.into(),
            seq,
        }
    }

    /// 从 canonical 执行来源派生归属事实。
    ///
    /// - [`crate::tool_invocation::InvocationOrigin::App`]：需要**已附着**的附件
    ///   （附件身份 + 连接纪元同时存在），否则返回 `None`——未附着的 App 没有
    ///   可路由的 endpoint，调用方据此 fail-closed，不得伪造归属；
    /// - [`crate::tool_invocation::InvocationOrigin::Host`]：直接派生；
    /// - 模型 / 嵌套来源：来源本身不含 session 与 turn 绑定，返回 `None`——调用方
    ///   必须用 [`Self::model_turn`] 显式提供真实 turn 事实。
    pub fn from_invocation_origin(
        origin: &crate::tool_invocation::InvocationOrigin,
    ) -> Option<Self> {
        match origin {
            crate::tool_invocation::InvocationOrigin::App {
                app_id,
                attachment_id,
                connection_epoch,
                ..
            } => Some(Self::app_attachment(
                app_id.clone(),
                attachment_id.clone()?,
                (*connection_epoch)?,
            )),
            crate::tool_invocation::InvocationOrigin::Host { namespace, seq } => {
                Some(Self::host_request(namespace.clone(), *seq))
            }
            crate::tool_invocation::InvocationOrigin::Model { .. }
            | crate::tool_invocation::InvocationOrigin::Nested { .. } => None,
        }
    }

    /// 归属 → 路由身份。**唯一**的映射实现（单一权威）。
    pub fn owner(&self) -> TrustedInteractionOwner {
        match self {
            Self::ModelTurn {
                session_id,
                binding,
            } => TrustedInteractionOwner::model(session_id.clone(), binding.clone()),
            Self::AppAttachment {
                app_id,
                attachment_id,
                connection_epoch,
            } => TrustedInteractionOwner::app(
                app_id.clone(),
                attachment_id.clone(),
                *connection_epoch,
            ),
            Self::HostRequest { namespace, seq } => {
                TrustedInteractionOwner::host(namespace.clone(), *seq)
            }
        }
    }

    /// 归属类别（稳定小写标签，用于日志与拒绝原因，不参与裁决）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ModelTurn { .. } => "model",
            Self::AppAttachment { .. } => "app",
            Self::HostRequest { .. } => "host",
        }
    }

    /// 归属绑定的 session；App / Host 归属无 session 事实（由附件 / 入口定位）。
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::ModelTurn { session_id, .. } => Some(session_id),
            Self::AppAttachment { .. } | Self::HostRequest { .. } => None,
        }
    }
}

/// 一次交互请求的**可信归属**（谁在问）。
///
/// 归属是路由输入：session 交互路由据此选择审批 endpoint、归属答复，并在
/// 后续批次据此消费 session entitlement（设计 §4.3）。归属只能由持有真实
/// 执行事实的调用方铸造，**不得**从请求参数自述、字符串前缀或 ID 拼接推断：
///
/// - [`Self::Model`]：真实模型 turn owner。事实来自 `TurnContext` /
///   [`ExecutionBinding`]（middleware 经 `BeforeToolState::execution_binding()`
///   投影，工具经 `ToolContext` 取），不新建平行身份。
/// - [`Self::App`]：Attachment + connectionEpoch；本轮只承载路由需要的三元
///   事实（完整 Attachment / RequestLedger 属阶段 3）。
/// - [`Self::Host`]：Host 程序化入口，字段与
///   [`crate::tool_invocation::InvocationOrigin::Host`] 同形（namespace + 同
///   namespace 内单调递增的 seq）。
///
/// 没有可信归属时调用方必须显式拒绝该交互（见
/// [`InteractionResponse::Unanswered`] 的 `NoOnlineEndpoint` 原因），不得伪造成
/// 上述任一归属。
///
/// # Examples
///
/// ```
/// use peri_acp_types::interaction::{ConnectionEpoch, TrustedInteractionOwner};
/// use peri_acp_types::session::{ExecutionBinding, TurnId};
/// use peri_acp_types::identity::AttemptId;
///
/// let binding = ExecutionBinding {
///     turn_id: TurnId::new(),
///     attempt_id: AttemptId::new(),
/// };
/// let owner = TrustedInteractionOwner::model("session-1", binding);
/// assert_eq!(owner.session_id(), Some("session-1"));
/// assert_eq!(owner.kind(), "model");
///
/// let app = TrustedInteractionOwner::app("app-1", "att-1", ConnectionEpoch::initial());
/// assert_eq!(app.session_id(), None, "App 归属的 session 由 Attachment 定位");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum TrustedInteractionOwner {
    /// 模型 turn 内的交互（工具审批 / 提问）。
    Model {
        /// 承载本次 turn 的 session。
        session_id: String,
        /// 事件纽带身份的 turn 部分。
        turn_id: TurnId,
        /// 本次 attempt 身份（同一 turn 的重试不复用）。
        attempt_id: AttemptId,
    },
    /// MCP App 宿主发起的交互。
    App {
        /// App 实例身份。
        app_id: String,
        /// 承载本次交互的连接附件。
        attachment_id: AttachmentId,
        /// 该 Attachment 的当前连接纪元。
        connection_epoch: ConnectionEpoch,
    },
    /// Host 程序化入口发起的交互（如 cron 触发的进入模型前审批）。
    Host {
        /// 宿主入口命名空间（如 `peri/cron/scheduled-trigger`）。
        namespace: String,
        /// 同一 namespace 内单调递增的调用序号。
        seq: u64,
    },
}

impl TrustedInteractionOwner {
    /// 从真实 turn 事实铸造模型归属。
    ///
    /// `binding` 必须来自 [`crate::session::TurnContext::execution_binding`]
    /// 或其投影（`InvocationContext`），不得手工拼装。
    pub fn model(session_id: impl Into<String>, binding: ExecutionBinding) -> Self {
        Self::Model {
            session_id: session_id.into(),
            turn_id: binding.turn_id,
            attempt_id: binding.attempt_id,
        }
    }

    /// 从 Attachment 事实铸造 App 归属。
    pub fn app(
        app_id: impl Into<String>,
        attachment_id: impl Into<AttachmentId>,
        connection_epoch: ConnectionEpoch,
    ) -> Self {
        Self::App {
            app_id: app_id.into(),
            attachment_id: attachment_id.into(),
            connection_epoch,
        }
    }

    /// 从 Host 入口事实铸造程序化归属。
    pub fn host(namespace: impl Into<String>, seq: u64) -> Self {
        Self::Host {
            namespace: namespace.into(),
            seq,
        }
    }

    /// 归属绑定的 session；[`Self::Host`] 无 session 事实（由入口自身定位），
    /// [`Self::App`] 的 session 由 Attachment 定位，故返回 `None`。
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Model { session_id, .. } => Some(session_id),
            Self::App { .. } | Self::Host { .. } => None,
        }
    }

    /// 归属类别（稳定小写标签，用于日志与拒绝原因，不参与裁决）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Model { .. } => "model",
            Self::App { .. } => "app",
            Self::Host { .. } => "host",
        }
    }

    /// 模型归属的 turn 事实；非模型归属返回 `None`。
    pub fn model_turn(&self) -> Option<(&str, &TurnId, &AttemptId)> {
        match self {
            Self::Model {
                session_id,
                turn_id,
                attempt_id,
            } => Some((session_id.as_str(), turn_id, attempt_id)),
            Self::App { .. } | Self::Host { .. } => None,
        }
    }
}

impl std::fmt::Display for TrustedInteractionOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model {
                session_id,
                turn_id,
                attempt_id,
            } => write!(
                f,
                "model(session={session_id}, turn={turn_id}, attempt={attempt_id})"
            ),
            Self::App {
                app_id,
                attachment_id,
                connection_epoch,
            } => write!(
                f,
                "app(app={app_id}, attachment={attachment_id}, epoch={connection_epoch})"
            ),
            Self::Host { namespace, seq } => write!(f, "host(namespace={namespace}, seq={seq})"),
        }
    }
}

// ─── UserInteractionBroker ─────────────────────────────────────────────────────

/// 统一人机交互 broker trait
///
/// 将 HITL（工具审批）和 AskUser（问答）两条路径统一为单一接口。
/// 应用层（TUI / CLI / 测试）实现此 trait，通过 `request` 方法挂起等待用户响应。
///
/// # 使用示例
///
/// ```rust,ignore
/// let broker: Arc<dyn UserInteractionBroker> = Arc::new(TuiInteractionBroker::new(tx));
/// let permission = PermissionMiddleware::with_shared_mode(
///     broker.clone(),
///     default_requires_approval,
///     Arc::new(SharedPermissionMode::new(PermissionMode::Default)),
///     None,
/// );
/// let ask_user_tool = AskUserTool::new(broker);
/// ```
///
/// # 归属与路由
///
/// 每个请求都带 [`TrustedInteractionOwner`]。实现方（ACP transport、session
/// 交互路由）按归属选择审批 endpoint 与归属答复；宿主入口是**可替换**的，
/// 不得把某一轮的固定 broker 缓存成 session 级权威，也不得在无法投递时降级为
/// 「自动批准」（应返回 [`InteractionResponse::Unanswered`] +
/// [`UnansweredCause`] 中**与事实相符**的那个变体：完全没有在线 endpoint 用
/// `NoOnlineEndpoint`，有 endpoint 但不匹配归属用 `OwnerUnmatched`，附件已
/// detach 用 `AttachmentDetached`）。
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    /// 发起一次人机交互，挂起直到用户响应。
    ///
    /// `owner` 由调用方从真实执行事实铸造（见 [`TrustedInteractionOwner`]）；
    /// 无法铸造可信归属的调用方必须显式拒绝，不得传伪造归属。
    async fn request(
        &self,
        owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse;
}

#[cfg(test)]
#[path = "interaction_test.rs"]
mod tests;
