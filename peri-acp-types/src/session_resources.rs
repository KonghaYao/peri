//! 会话资源门面 — 消费侧唯一的会话行为契约。
//!
//! 业务侧（Agent / ACP / Controller / TUI）只依赖本 trait 的行为语义，注入的是
//! `Arc<dyn SessionResources>`。数据的持久化位置（本机 SQLite / Turso Cloud）与
//! 本机执行事实（发现、绑定、owner、dirty、排空）由资源内部两面分别实现，
//! 数据端口不向业务暴露事务、CAS、SQL batch、连接或补偿令牌。
//!
//! 三条必须成立的约束：
//!
//! 1. **不提供 no-op 默认实现。** 每个行为都是必需方法：无法完成的行为必须返回
//!    明确失败，不能让调用方把「不支持」当成「没有数据」。
//! 2. **声明即保证。** 行为要么满足完整后置条件，要么在副作用前失败；声明
//!    [`DataCapabilities::Complete`] 的 adapter 不得对任何行为退化为 no-op。
//! 3. **效果确定性单独表达。** 失败原因与「是否已生效」分别建模，见
//!    [`SessionResourceError`] 与 [`MutationOutcome`]。
//!
//! 只读入口（`AccessMode::ReadOnly`）不得靠创建一个临时未绑定会话绕过执行授权。
//! 旧 `ThreadStore`（[`crate::store::ThreadStore`]）是迁移桥，见其模块文档。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::messages::MessageId;
use crate::store::{CompactionChange, InheritedContext, MessageFlags, PersistedPayload};
use crate::thread::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta};
use crate::workspace::{
    ReadOnlyAdmission, RecoveryRequiredDetails, ResetDirtyRequest, ResolvedWorkspace,
    ScopedThreadPage, ScopedThreadQuery, SessionBinding, SessionExecutionLease, WorkspaceError,
};

// ─── 能力与准入 ────────────────────────────────────────────────────────────────

/// 本次打开实际取得的读写权限 — 配置/授权事实，不推导数据能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

/// 后端能安全完成的会话行为面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataCapabilities {
    /// §行为清单全部满足完整后置条件。
    Complete,
    /// 只支持读取历史；所有 mutation 在副作用前返回 `Unsupported`。
    HistoryReadOnly,
}

/// 本机执行资格 — 与数据能力、与访问模式都独立。
///
/// 三种事实互不推导：`ReadOnly` 不蕴含 `HistoryReadOnly`（只读授权下数据能力仍可
/// 为 `Complete`，只是本次不允许写）；`Complete` 不蕴含可执行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionAvailability {
    Available,
    /// 会话没有可验证的执行绑定。
    BindingMissing,
    /// 绑定的工作目录在本机不可用或已变化。
    WorkspaceUnavailable,
    /// 执行所有权在别处。
    OwnedElsewhere,
    /// 上次执行未干净收尾：需要按精确代际确认后解除。
    Dirty(RecoveryRequiredDetails),
    /// 存在无法证明终态的持久化写入：先收敛再执行。
    PersistencePending,
    /// 本次打开只读，无法取得执行所有权。
    ReadOnlyStore,
    /// 本机执行在该后端不支持。
    Unsupported,
}

/// 一次诊断读取：本次打开的权限、后端能力面，以及（给定会话时的）执行资格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAvailability {
    pub access: AccessMode,
    pub capabilities: DataCapabilities,
    /// `None` 表示未指定会话，本次没有查询会话级执行事实。
    pub execution: Option<ExecutionAvailability>,
}

/// 未决持久化的收敛结果：可重载，或仍阻塞。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceRecovery {
    /// 已收敛，会话可重新加载并继续写入。
    Recovered,
    /// 仍无法证明上次写入的终态：保持阻塞，调用方不得继续写。
    StillBlocked,
}

// ─── 领域输入 ──────────────────────────────────────────────────────────────────

/// 会话创建时冻结的版本化上下文快照（opaque 字节，沿用现有 JSON envelope）。
///
/// 构建与解码归 ACP frozen owner（`session::{frozen,frozen_snapshot}`）；存储只
/// 原样保存与返回，不解释内容、不因版本变化丢弃数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenSnapshotBytes(String);

impl FrozenSnapshotBytes {
    pub fn new(bytes: impl Into<String>) -> Self {
        Self(bytes.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// 新会话的初始 metadata。
///
/// 不携带 `message_count` / `updated_at`：
/// 这些是行为维护的派生事实，构建方给值只会制造第二个真相。
#[derive(Clone, Debug)]
pub struct NewSessionMeta {
    pub title: Option<String>,
    pub cwd: String,
    pub parent_thread_id: Option<ThreadId>,
    pub hidden: bool,
    pub cancel_policy: CancelPolicy,
    pub snapshot_at_message_id: Option<MessageId>,
}

/// 固定身份的待创建会话：ID 与创建时间在构建时一次生成，重试不重建。
#[derive(Clone, Debug)]
pub struct NewSession {
    pub thread_id: ThreadId,
    /// RFC3339 创建时间，构建时生成一次。
    pub created_at: String,
    pub meta: NewSessionMeta,
    /// 不可变执行绑定。
    pub binding: SessionBinding,
    pub frozen: FrozenSnapshotBytes,
}

/// 未发布创建（J2 两阶段）：身份/绑定/执行代际已成立，frozen 尚未提交。
///
/// 与 [`NewSession`] 的唯一区别是**没有 frozen**：内容准入（资源读取后定稿）发生在取得
/// 执行所有权之后，因此第一阶段只能写「除 frozen 以外」的全部事实。草稿不对外可见
/// （列表按 `message_count > 0` 过滤），cold load 命中半写草稿按 typed 错误 fail-closed，
/// 清理判据见 [`SessionResources::discard_incomplete_initialization`]。
#[derive(Clone, Debug)]
pub struct NewSessionDraft {
    pub thread_id: ThreadId,
    /// RFC3339 创建时间，构建时生成一次。
    pub created_at: String,
    pub meta: NewSessionMeta,
    /// 不可变执行绑定。
    pub binding: SessionBinding,
}

/// fork 目标：目标身份 + source 截止点 + 已完成 ID 重映射的 payload/flags。
///
/// source 只读；ID 重映射由 [`crate::store::history::remap_fork_history`] 在构建时
/// 完成，adapter 不再执行一遍 fork 算法。
#[derive(Clone, Debug)]
pub struct ForkSnapshot {
    pub target: NewSession,
    pub source_id: ThreadId,
    pub payloads: Vec<PersistedPayload>,
    pub flags: HashMap<MessageId, MessageFlags>,
}

/// child 目标：父子/根归属 + 继承快照。
///
/// frozen 由不可变 parent/root 关系解析并复制原字节，不重新扫描目录；inherited
/// 保存 child 创建时刻的继承区与 flags，不得用父会话当前 flags 顶替。
///
/// 不变量：父子关系只允许有一个真相——`target.meta.parent_thread_id` 必须等于 `parent_id`
/// （落库用的是前者），`parent_id`/`root_id` 都不得等于 `target.thread_id`。不满足的快照
/// 是调用方的错误，必须在产生任何副作用之前被拒绝。
#[derive(Clone, Debug)]
pub struct ChildSnapshot {
    pub target: NewSession,
    pub parent_id: ThreadId,
    pub root_id: ThreadId,
    pub inherited: InheritedContext,
}

// ─── 读取结果 ──────────────────────────────────────────────────────────────────

/// 会话的执行绑定分类。
///
/// `None` 不再同时表达 legacy、不支持和损坏：损坏与版本不支持是错误
/// （[`SessionResourceError`]），不是状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingState {
    /// 已按本机 workspace 绑定。
    Bound(SessionBinding),
    /// 无绑定，但按本机规则确认是 legacy 历史（来源与记录状态联合判定）。
    LegacyConfirmed,
    /// 外来会话或本机登记缺失：历史可读，但不得当作 legacy 自动接纳。
    ExternalOrUnregistered,
    /// 既无绑定也无 legacy 证据。
    Missing,
}

/// 会话 frozen 快照的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrozenState {
    Present(FrozenSnapshotBytes),
    /// legacy 会话尚未持久化快照；由既有 legacy 规则决定能否补齐。
    LegacyAbsent,
    /// 存在快照但本构建读不懂（版本/形状不支持）——不是「缺失」。
    Unsupported,
}

/// 绑定复核的力度：两次复核的差别只在是否重跑一次完整发现。
///
/// 与「事务」「CAS」无关，也不表达复核结果的确定性差异：两者都在不一致时失败，
/// `Recorded` 只是省去外部进程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingRecheck {
    /// 权威复核：SQL 关系、关键文件对象，加一次完整发现快照比对。
    Full,
    /// 同一次准入内的复核：SQL 关系与关键文件对象，不启动外部进程。
    Recorded,
}

/// 一次一致读取的会话视图。
///
/// 不含 pool、缓存 epoch、事务状态或执行 handle：这些是实现细节或另一面的事实。
#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub meta: ThreadMeta,
    pub binding: BindingState,
    pub frozen: FrozenState,
    pub payloads: Vec<PersistedPayload>,
    pub flags: HashMap<MessageId, MessageFlags>,
    pub inherited: InheritedContext,
}

// ─── 定向更新 ──────────────────────────────────────────────────────────────────

/// rewind 边界：区分「保留到目标」与「从目标开始移除」。
///
/// transcript rewind 保留目标本身，ACP 用户 rewind 移除目标及以后；两者语义不可
/// 合并，合并会把「保留到目标」失真成「删掉目标」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindBoundary {
    KeepThrough(MessageId),
    RemoveFrom(MessageId),
}

impl RewindBoundary {
    pub fn message_id(&self) -> MessageId {
        match self {
            Self::KeepThrough(id) | Self::RemoveFrom(id) => *id,
        }
    }
}

/// 定向 metadata 更新：`None` 表示不更新，`Some(None)` 表示清除。
///
/// 不提供整份 [`ThreadMeta`] 覆盖入口：cwd、binding、父子身份、计数与缓存都不是
/// 调用方可以借更新顺手改掉的字段。
#[derive(Clone, Debug, Default)]
pub struct SessionMetaPatch {
    pub title: Option<Option<String>>,
    pub status: Option<AgentStatus>,
    pub cancel_policy: Option<CancelPolicy>,
    /// 会话配置 JSON 快照。
    pub config: Option<Option<String>>,
}

// ─── 结果与错误 ────────────────────────────────────────────────────────────────

/// 副作用的确定性 — 与失败原因分开建模。
///
/// 不从 `Timeout` / `Unavailable` 自动推导「未生效」：那两种原因本身不回答
/// 「写进去了没有」，只能由 adapter 在能证明时给出 `NotApplied`。
///
/// 与 A §5.4 的三态一致；那里的 `NotAppliedReason` / `UnknownReason` 由
/// [`SessionResourceErrorKind`] 承载（原因是原因，效果是效果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOutcome {
    /// 已确认未生效。
    NotApplied,
    /// 已确认生效（例如数据已保存、执行准入失败）。
    Applied,
    /// 无法证明生效与否：热态必须失效并阻塞续写与 clean。
    Unknown,
}

/// 失败原因。不携带 SQL、token、事务 ID 或连接细节。
#[derive(Debug)]
pub enum SessionResourceErrorKind {
    InvalidInput {
        detail: String,
    },
    NotFound,
    /// 后端能力面不支持该行为（含只读能力面下的 mutation）。
    Unsupported,
    /// 本次打开只有读权限。
    ReadOnlyStore,
    /// 本机 workspace / binding / owner 语义，保持 [`WorkspaceError`] 的分类。
    Workspace(WorkspaceError),
    /// 记录存在但无法解释（损坏、格式不可读）。
    Corrupt {
        detail: String,
    },
    /// 后端暂不可用（网络、锁、IO）：可重试，但不代表没生效。
    Unavailable {
        detail: String,
    },
    Timeout,
    /// 无法证明上一次写入的终态；携带可定位的 session/root 关联。
    PersistenceUncertain {
        thread_id: Option<ThreadId>,
    },

    /// 数据已完整保存，但执行准入失败：不是「确定未创建」。
    SavedButNotAdmitted {
        thread_id: ThreadId,
    },
    /// 现有状态与本次请求冲突（一次性提交重复调用、对已定稿草稿撤销、清理判据不成立）。
    ///
    /// 与 [`Self::InvalidInput`] 分开：输入本身合法，冲突来自存储里已经成立的事实，
    /// 调用方据此知道「不要重试，先重读」。
    Conflict {
        detail: String,
    },
    Internal {
        detail: String,
    },
}

/// 会话行为失败：原因 + 效果确定性。
///
/// `effect` 由构造器从 `kind` 派生，调用方无法拼出互相矛盾的组合：
/// `Unknown` 只对应未决持久化，`Applied` 只对应「已保存但准入失败」。
#[derive(Debug)]
pub struct SessionResourceError {
    kind: SessionResourceErrorKind,
    effect: MutationOutcome,
}

/// 会话行为的统一返回类型。
pub type SessionResourceResult<T> = std::result::Result<T, SessionResourceError>;

impl SessionResourceError {
    pub fn new(kind: SessionResourceErrorKind) -> Self {
        let effect = match &kind {
            SessionResourceErrorKind::PersistenceUncertain { .. } => MutationOutcome::Unknown,
            SessionResourceErrorKind::SavedButNotAdmitted { .. } => MutationOutcome::Applied,
            _ => MutationOutcome::NotApplied,
        };
        Self { kind, effect }
    }

    /// 无法证明写入终态：门面据此使热态失效并阻塞续写/clean。
    pub fn persistence_uncertain(thread_id: Option<ThreadId>) -> Self {
        Self::new(SessionResourceErrorKind::PersistenceUncertain { thread_id })
    }

    /// 数据已完整保存，但执行准入失败；包含可定位的 session identity。
    pub fn saved_but_not_admitted(thread_id: ThreadId) -> Self {
        Self::new(SessionResourceErrorKind::SavedButNotAdmitted { thread_id })
    }

    /// 本次请求与已成立的存储事实冲突（不覆盖、不删除、不重试）。
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(SessionResourceErrorKind::Conflict {
            detail: detail.into(),
        })
    }

    pub fn kind(&self) -> &SessionResourceErrorKind {
        &self.kind
    }

    pub fn effect(&self) -> MutationOutcome {
        self.effect
    }

    pub fn is_persistence_uncertain(&self) -> bool {
        matches!(
            self.kind,
            SessionResourceErrorKind::PersistenceUncertain { .. }
        )
    }

    /// 仅当失败来源于本机 workspace 语义时给出原错误，供只读降级等既有映射使用。
    pub fn workspace_error(&self) -> Option<&WorkspaceError> {
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => Some(error),
            _ => None,
        }
    }

    /// 只读准入原因：本次没能取得执行所有权，但历史仍可按只读会话进入。
    ///
    /// 三种既有 workspace 原因原样保留；只读存储（[`SessionResourceErrorKind::ReadOnlyStore`]）
    /// 归入同一集合——「本节点给不出执行所有权、历史可读」是同一件事，消费侧的降级路径
    /// 因此不必按错误种类分支。
    ///
    /// 不在本集合内的原因不降级：`ReadOnlyStore` 的 workspace 变体（连会话都还没有的
    /// 登记失败）与 `PersistenceUncertain`（终态未证明）都保持原样上报。
    pub fn read_only_admission(&self) -> Option<ReadOnlyAdmission> {
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => {
                ReadOnlyAdmission::from_workspace_error(error)
            }
            SessionResourceErrorKind::ReadOnlyStore => {
                Some(ReadOnlyAdmission::ExecutionLeaseRequired)
            }
            _ => None,
        }
    }
}

impl From<WorkspaceError> for SessionResourceError {
    fn from(error: WorkspaceError) -> Self {
        Self::new(SessionResourceErrorKind::Workspace(error))
    }
}

impl std::fmt::Display for SessionResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (reason, detail) = match &self.kind {
            SessionResourceErrorKind::InvalidInput { detail } => ("invalid input", detail.as_str()),
            SessionResourceErrorKind::NotFound => ("session not found", ""),
            SessionResourceErrorKind::Unsupported => ("behavior is unsupported here", ""),
            SessionResourceErrorKind::ReadOnlyStore => ("session store is read-only", ""),
            SessionResourceErrorKind::Workspace(_) => ("workspace check failed", ""),
            SessionResourceErrorKind::Corrupt { detail } => {
                ("session data is corrupt", detail.as_str())
            }
            SessionResourceErrorKind::Unavailable { detail } => {
                ("store is unavailable", detail.as_str())
            }
            SessionResourceErrorKind::Timeout => ("store operation timed out", ""),
            SessionResourceErrorKind::PersistenceUncertain { .. } => {
                ("persistence outcome is unknown; reload the session", "")
            }
            SessionResourceErrorKind::SavedButNotAdmitted { .. } => {
                ("session data was saved but execution admission failed", "")
            }
            SessionResourceErrorKind::Conflict { detail } => {
                ("state conflicts with request", detail.as_str())
            }
            SessionResourceErrorKind::Internal { detail } => {
                ("internal storage error", detail.as_str())
            }
        };
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => write!(f, "{reason}: {error}"),
            _ if detail.is_empty() => {
                write!(f, "{reason} ({:?})", self.effect)
            }
            _ => write!(f, "{reason}: {detail}"),
        }
    }
}

impl std::error::Error for SessionResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => Some(error),
            _ => None,
        }
    }
}

// ─── 门面 ──────────────────────────────────────────────────────────────────────

/// 未发布创建的句柄（J2 两阶段的第二阶段）：frozen 一次性提交，或整条草稿被撤销。
///
/// 语义（与失败补偿矩阵同源）：
///
/// - 提交是 **write-once CAS**：仅当这条 identity 从未提交过 frozen 时生效；重复调用返回
///   typed 冲突（[`SessionResourceErrorKind::Conflict`]），不覆盖已提交字节；
/// - 提交在同一事务内校验本进程仍是该 identity 的活 owner、且执行代际尚未结清；
/// - [`Self::abandon`] 只撤销**未提交**的草稿（幂等）；已提交 frozen 的草稿必须拒绝删除
///   ——那会销毁一个「已定稿、未发布」的合法中间态；
/// - 发布（`sessions.insert`）之后，句柄让位给会话持有的 [`SessionExecutionLease`]，
///   由关闭路径在资源全部停止之后 `mark_clean`。
#[async_trait]
pub trait SessionInitialization: Send + Sync {
    fn thread_id(&self) -> &ThreadId;

    /// 本次草稿的执行所有权；发布后由会话持有（关闭时 `mark_clean` 用它）。
    fn execution_lease(&self) -> Arc<dyn SessionExecutionLease>;

    /// 一次性提交 frozen：仅当该 identity 从未提交过；重复调用返回 typed 冲突，不覆盖。
    async fn commit_frozen(&self, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()>;

    /// 撤销未发布的创建（幂等）；已提交后调用返回 typed 冲突，不删除。
    async fn abandon(self: Arc<Self>) -> SessionResourceResult<()>;
}

/// 子会话 resume 认领 handle。
///
/// 认领期间的 active/原状态/终态由资源内部保存，调用方只提交领域结果，不拼补偿
/// 写入、不接触事务或重试令牌。
#[async_trait]
pub trait ChildResumeClaim: Send + Sync {
    /// 认领成功并开始运行。
    async fn mark_running(&self) -> SessionResourceResult<()>;
    /// 移交后台执行（仍属本次认领）。
    async fn hand_off_to_background(&self) -> SessionResourceResult<()>;
    /// 认领后准备失败：恢复到认领前的状态，不留 active 残留。
    async fn mark_failed(&self) -> SessionResourceResult<()>;
    /// 认领终止（取消/宿主退出）：恢复到认领前的状态。
    async fn mark_terminated(&self) -> SessionResourceResult<()>;
}

/// 会话资源门面：业务侧唯一的会话行为入口。
///
/// 每个方法都要么满足后置条件，要么在产生副作用前返回明确失败；没有默认实现可用
/// 来冒充成功。实现者必须同时保证：
///
/// - mutation 先检查本 root 的有效 owner 与未决持久化，再落盘；
/// - `AccessMode::ReadOnly` 下所有 mutation 返回 [`SessionResourceErrorKind::ReadOnlyStore`]；
/// - 结果不确定时返回 [`SessionResourceError::persistence_uncertain`]，不得重试后伪装成功。
#[async_trait]
pub trait SessionResources: Send + Sync {
    fn oauth_credentials(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::oauth_credentials::OAuthCredentialPort>> {
        None
    }
    // ── 能力与准入 ──

    /// 本次打开的权限、后端能力面；给定 `session` 时附带该会话的执行资格。
    ///
    /// 只暴露行为面的能力，不暴露事务、CAS 或隔离级别。
    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability>;

    /// 解析并登记一个本机执行目录。
    async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace>;

    /// 校验会话与给定 workspace 的绑定关系；不取得执行所有权。
    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()>;

    /// 建立当前机器的运行生命周期句柄，不认领持久会话的唯一所有权。
    ///
    /// 拒绝跨机器环境执行与只读打开；旧 dirty 或其他实例恢复不构成认领门槛。
    async fn acquire_execution(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 解除精确代际的本机 dirty；永远不解除未决持久化，也不接受把普通 dirty 映射成 reset。
    async fn reset_dirty_execution(&self, request: &ResetDirtyRequest)
        -> SessionResourceResult<()>;

    // ── 创建与接纳 ──

    /// 创建新会话：meta/binding/frozen 完整保存后返回执行准入。
    ///
    /// 数据已保存但准入失败时返回
    /// [`SessionResourceError::saved_but_not_admitted`]，不得报告成「确定未创建」；
    /// 未发布创建的撤销由门面内部承担（见 [`Self::abandon_initialization`]）。
    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 撤销本次尚未发布的创建（不是通用 rollback，不修改既有 source 会话）。
    ///
    /// 语义由**入口**决定：本方法是 write-once 完整创建（fork 等一次性创建）的失败补偿
    /// ——目标创建即带 frozen、可由 source 重生成，撤销就是把它整条删掉，不叠加「未提交
    /// frozen」判据。两阶段草稿的撤销走
    /// [`SessionInitialization::abandon`]（那里的 `frozen IS NULL` 判据约束已定稿草稿）。
    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()>;

    /// 未发布创建的第一阶段：写身份/绑定/执行代际（frozen 暂空）并取得执行所有权。
    ///
    /// 内容准入（资源读取后定稿 frozen）需要所有权先成立，因此创建被拆成两段：本方法返回
    /// 的句柄给出 [`SessionInitialization::commit_frozen`] 与
    /// [`SessionInitialization::abandon`] 两个终局动作，二者互斥且各自一次性。
    ///
    /// 未提交的草稿不可执行、不出现在列表；同 identity 并发只有一个 owner（本机 sidecar 锁
    /// 与执行代际唯一性）。
    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>>;

    /// 半写草稿的检测与清理（崩溃恢复，不会自动删除「已提交未发布」的会话）。
    ///
    /// 判据：绑定已成立 **且** `frozen IS NULL`（与 legacy「无绑定且无代际行」互斥）；
    /// 目标进程内无活 owner、且能取得 OS 锁时才删除。已提交 frozen 的会话返回 typed 冲突
    /// ——它已有定稿快照，走既有 dirty 恢复（`RecoveryRequired` → 显式 `reset_dirty`），
    /// 不在这里被销毁。
    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 接纳已确认的本机 legacy 会话：binding 与缺失的 frozen 一起成立。
    ///
    /// 竞争时返回胜者事实，不返回 CAS 布尔值；`saved_cwd` 必须是会话保存的原始
    /// 路径（不允许调用方借接纳顺手改绑）。
    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()>;

    // ── 读取 ──

    /// 一次一致读取：metadata、binding 分类、frozen 状态、own payload/flags、inherited。
    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot>;

    /// 轻量绑定分类：只回答本机能否验证这条会话的绑定，不加载历史、不启动发现。
    ///
    /// 与 [`Self::load_session_snapshot`] 的同一套分类规则，供只关心绑定/workspace 的
    /// 读取（`session/context`、metadata 之外的准入身份）使用；会话不存在是
    /// [`SessionResourceErrorKind::NotFound`]，损坏与版本不支持是错误，不冒充状态。
    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState>;

    /// 按会话 ID 读取保存的工作区定位信息，不做目录归属认领或改绑。
    ///
    /// 目录存在性属于工具执行资格，不阻断历史定位；不依赖调用方当前目录。
    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace>;

    /// 完整逻辑上下文（继承区在前、自有 payload 在后）：历史回放与 `metadata(history)`
    /// 的唯一读取，不返回派生缓存。
    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>>;

    /// 小型 metadata 投影；不加载历史或大快照。
    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta>;
    async fn session_environment_id(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        Ok(None)
    }

    /// 按 scope/cursor/limit 分页列举；过滤在数据端完成。
    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage>;

    /// 直接子会话 metadata。
    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;

    /// 以 `root` 为根的整棵会话树 metadata（含自身）。
    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;

    // ── 写入 ──

    /// 追加 canonical payload 批次。
    ///
    /// 顺序稳定、计数与自动标题一致维护；id 已存在或批次内重复必须失败，
    /// 不得静默忽略（见 [`crate::store::history::ensure_distinct_ids`]）。
    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()>;

    /// 保存 fork 目标快照；source 不变。
    async fn save_fork(
        &self,
        fork: &ForkSnapshot,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 保存 child：继承区与父子关系一起成立，沿用已存在的 root owner。
    ///
    /// 父子关系在快照里出现两次（[`ChildSnapshot::parent_id`] 与
    /// [`NewSessionMeta::parent_thread_id`]），实现必须在写入前要求两者一致；不一致、
    /// 自指父关系、把自己当根都必须拒绝，且拒绝不留任何写入（详见 `ChildSnapshot` 不变量）。
    async fn save_child(
        &self,
        child: &ChildSnapshot,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()>;

    /// 在有效根 owner 下串行认领 child resume。
    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>>;

    /// 应用一次 compaction 变更：摘要、flags、计数与缓存视图全部生效或全部不生效。
    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()>;

    /// 应用投影/flags 变更集，并由资源统一维护派生缓存视图。
    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()>;

    /// 按显式边界 rewind；派生计数与缓存同步更新。
    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()>;

    /// 按 id 集合精确移除历史条目。
    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()>;

    /// 定向更新 metadata（标题/状态/取消策略/配置），不整份覆盖。
    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()>;

    /// 删除整棵会话树。
    ///
    /// 数据删除一致完成；执行关闭与未决持久化证据不得被级联提前抹掉
    /// （删除是刻意行为这一事实必须留下）。
    async fn delete_session_tree(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 收敛未决持久化：adapter 内部完成，调用方不提供 operation token。
    async fn recover_session_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery>;

    /// 排空该会话已排队的持久化写入（有界等待）。
    async fn drain_persistence(&self, id: &ThreadId) -> SessionResourceResult<()>;
}

/// 部署生命周期关闭端口：关闭整个会话存储的**唯一**入口。
///
/// 门面（[`SessionResources`]）是业务句柄：可以克隆、可以注入 Agent/Controller/
/// middleware，它只提供会话行为与必要的收敛能力（恢复、排空）。**关闭整个存储不是
/// 业务行为**——那会让任何持有句柄的调用方获得全局销毁权，因此关闭权单独由本端口
/// 承载，并由部署装配（TUI/print/stdio 的宿主配置）在**自己的任务排空之后**调用一次。
///
/// 实现必须把两件事分开：
///
/// - 关闭**不可逆地**停止新写入，但「停止新写入」不等于「已关闭」：只有真实检查
///   （在途写入、未决持久化）全部结清并确实关闭数据面之后才确认关闭，此后的重复调用
///   才幂等成功；
/// - 失败或被取消的关闭不构成确认，重复调用必须重新检查。未确认期间该存储的恢复与
///   排空仍然可用（业务句柄不受影响），否则第一次关闭失败就再也没有收敛路径。
#[async_trait]
pub trait SessionStoreShutdownPort: Send + Sync {
    /// 排空之后关闭会话存储；只有确认关闭才返回 `Ok`。
    async fn shutdown(&self) -> SessionResourceResult<()>;
}

#[cfg(test)]
#[path = "session_resources_test.rs"]
mod tests;
