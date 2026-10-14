//! 会话资源门面 — 消费侧唯一的会话行为契约。
//!
//! 业务侧（Agent / ACP / Controller / TUI）只依赖本 trait 的行为语义，注入的是
//! `Arc<dyn SessionResources>`。数据的持久化位置（本机 SQLite / Turso Cloud）与
//! 本机执行事实（发现、绑定、排空）由资源内部两面分别实现，
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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::history_page::{
    HistoryCursor, HistoryCursorError, HistoryForwardCursor, HistoryPage, HistoryPageBounds,
    HistorySegment,
};
use crate::messages::MessageId;
use crate::store::{CompactionChange, InheritedContext, MessageFlags, PersistedPayload};
use crate::system_reminder::TrustedSystemReminder;
use crate::thread::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta};
use crate::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, WorkspaceError,
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
    /// 存在无法证明终态的持久化写入：先收敛再执行。
    PersistencePending,
    /// 本次打开只读，无法写入会话。
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

/// 未发布创建（J2 两阶段）：身份/绑定已保存、运行句柄已登记，frozen 尚未提交。
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
///
/// 未知版本**不在这里表达**：格式知识属于 ACP 侧解码器
/// （`FrozenSnapshotError::UnsupportedVersion`），存储层只区分「有字节」与
/// 「legacy 缺失」，不为制造枚举用法去理解 snapshot 格式。
pub enum FrozenState {
    Present(FrozenSnapshotBytes),
    /// legacy 会话尚未持久化快照；由既有 legacy 规则决定能否补齐。
    LegacyAbsent,
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

/// 执行关键的有效视图：恢复后真正要跑的那一份上下文。
///
/// 与 [`SessionSnapshot`] 的差别只在**范围**，不在一致性要求：两者都是一次一致读取。
/// 被 Full/Smart compact 排除的 own 条目**不在这里返回**——它们仍存在于存储中，
/// 完整历史（含已排除正文）是另一条接口，见 [`SessionResources::load_session_history`]。
///
/// 排除与投影判定属于存储事实，由后端在读取时给出：调用方直接消费这里的集合语义，
/// 不重新推导「哪些条目还在有效范围里」。
#[derive(Clone, Debug)]
pub struct ActiveSessionSnapshot {
    pub meta: ThreadMeta,
    pub binding: BindingState,
    pub frozen: FrozenState,
    pub inherited: InheritedContext,
    /// 仅未排除的 own 条目，顺序与 [`SessionSnapshot::payloads`] 一致。
    pub payloads: Vec<PersistedPayload>,
    /// 仅活跃 id 的非默认 flags：已排除条目的标记不返回。
    pub flags: HashMap<MessageId, MessageFlags>,
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

/// rewind 提交的**实际效力**：边界是否真的在存储里生效。
///
/// 「未知边界保持无变更」是历史语义（目标不在本会话时既不删也不清零），但它是**没有
/// 生效**，不是「生效了」：调用方据此决定能不能把内存热态改写成「已截断 + 已清零」的
/// 那份。两者分开表达，存储与热态才不会分叉。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindOutcome {
    /// 边界成立：删除与清零都在同一次提交里落定。
    Applied,
    /// 边界目标不在本会话历史里：本次没有删除、也没有清零，存储保持原样。
    BoundaryMissing,
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
    /// 本机 workspace / binding 语义，保持 [`WorkspaceError`] 的分类。
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
    /// 历史分页游标不能用于本次读取：分类见 [`HistoryCursorError`]。
    ///
    /// 单独一类是为了让出口把「坏游标」映射成参数错误、把「失效游标」映射成需要重新
    /// 开始的错误；两者都不得降级成首屏或空页。
    Cursor(HistoryCursorError),
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

/// Readback of an uncertain close-intent removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseSettlement {
    Pending,
    Finished,
    Unknown,
}

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
            SessionResourceErrorKind::Cursor(_) => ("history cursor is unusable", ""),
            SessionResourceErrorKind::Internal { detail } => {
                ("internal storage error", detail.as_str())
            }
        };
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => write!(f, "{reason}: {error}"),
            SessionResourceErrorKind::Cursor(error) => write!(f, "{reason}: {error}"),
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
/// - 提交前检查未决持久化，执行唯一性由 peri-sdk 负责；
/// - [`Self::abandon`] 只撤销**未提交**的草稿（幂等）；已提交 frozen 的草稿必须拒绝删除
///   ——那会销毁一个「已定稿、未发布」的合法中间态；
#[async_trait]
pub trait SessionInitialization: Send + Sync {
    fn thread_id(&self) -> &ThreadId;

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
    /// 移交后台执行（仍属本次认领）；完成后调用方才可放行后台执行。
    async fn hand_off_to_background(&self) -> SessionResourceResult<()>;
    /// 认领后准备失败：恢复到认领前的状态，不留 active 残留。
    /// 移交失败或移交后启动被取消也可补偿，但调用方须确保后台执行尚未放行。
    async fn mark_failed(&self) -> SessionResourceResult<()>;
    /// 认领终止（取消/宿主退出）：恢复到认领前的状态。
    async fn mark_terminated(&self) -> SessionResourceResult<()>;
}

/// 会话资源门面：业务侧唯一的会话行为入口。
///
/// 每个方法都要么满足后置条件，要么在产生副作用前返回明确失败；没有默认实现可用
/// 来冒充成功。实现者必须同时保证：
///
/// - mutation 先检查访问权限与未决持久化，再落盘；
/// - `AccessMode::ReadOnly` 下所有 mutation 返回 [`SessionResourceErrorKind::ReadOnlyStore`]；
/// - 结果不确定时返回 [`SessionResourceError::persistence_uncertain`]，不得重试后伪装成功。
#[async_trait]
pub trait SessionResources: Send + Sync {
    fn oauth_credentials(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::oauth_credentials::OAuthCredentialPort>> {
        None
    }
    /// 返回已绑定单个 Workspace 的 OAuth 能力；调用方须先取得可信会话归属。
    fn oauth_credentials_for_workspace(
        &self,
        _workspace_id: crate::workspace::WorkspaceId,
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

    /// 清除已完成资源排空的会话关闭意图。
    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()>;

    async fn close_settlement(&self, id: &ThreadId) -> SessionResourceResult<CloseSettlement>;

    // ── 创建与接纳 ──

    /// 创建新会话：meta/binding/frozen 完整保存后返回执行准入。
    ///
    /// 数据已保存但准入失败时返回
    /// [`SessionResourceError::saved_but_not_admitted`]，不得报告成「确定未创建」；
    /// 未发布创建的撤销由门面内部承担（见 [`Self::abandon_initialization`]）。
    /// 同 ID 的不可变创建事实一致时可重复准入，不覆盖历史、配置、标题、状态或 frozen；
    /// binding、创建时间、cwd、父关系、快照截止点或 frozen 不一致时拒绝。
    async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()>;

    /// 撤销本次尚未发布的创建（不是通用 rollback，不修改既有 source 会话）。
    ///
    /// 语义由**入口**决定：本方法是 write-once 完整创建（fork 等一次性创建）的失败补偿
    /// ——目标创建即带 frozen、可由 source 重生成，撤销就是把它整条删掉，不叠加「未提交
    /// frozen」判据。两阶段草稿的撤销走
    /// [`SessionInitialization::abandon`]（那里的 `frozen IS NULL` 判据约束已定稿草稿）。
    async fn abandon_initialization(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 未发布创建的第一阶段：保存身份/绑定（frozen 暂空）并取得初始化句柄。
    ///
    /// 内容准入（资源读取后定稿 frozen）分成两段：本方法返回
    /// 的句柄给出 [`SessionInitialization::commit_frozen`] 与
    /// [`SessionInitialization::abandon`] 两个终局动作，二者互斥且各自一次性。
    ///
    /// 未提交的草稿不出现在列表；初始化提交不认领执行所有权。
    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>>;

    /// 半写草稿的检测与清理（崩溃恢复，不会自动删除「已提交未发布」的会话）。
    ///
    /// 判据：绑定已成立 **且** `frozen IS NULL`（与 legacy「无绑定且无 frozen」互斥）；
    /// 无未决写时才删除。已提交 frozen 的会话返回 typed 冲突
    /// ——它已有定稿快照，可按 ID 重新建立执行准入，
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

    /// 一次一致读取的执行关键有效视图：同一份 metadata/binding/frozen/inherited，
    /// 但 `payloads` 只含未排除的 own 条目、`flags` 只含活跃 id。
    ///
    /// 默认实现是**完整读取再过滤**的回退路径，供测试替身等非生产后端使用；它不满足
    /// REQ-T03「恢复路径不得先全量再过滤」。**生产后端必须覆写**本方法，让过滤发生在
    /// 存储层（与 [`Self::load_session_history`] 同样的读取条件加上未排除约束），
    /// 使已排除正文根本不进入内存。
    async fn load_active_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ActiveSessionSnapshot> {
        let SessionSnapshot {
            meta,
            binding,
            frozen,
            payloads,
            flags,
            inherited,
        } = self.load_session_snapshot(id).await?;
        // 排除只认 flags 事实：没有标记的 payload 视为未排除，不猜测。
        let excluded: HashSet<MessageId> = flags
            .iter()
            .filter(|(_, flags)| flags.excluded)
            .map(|(id, _)| *id)
            .collect();
        let mut flags = flags;
        flags.retain(|id, _| !excluded.contains(id));
        let payloads = payloads
            .into_iter()
            .filter(|payload| !excluded.contains(&payload.id()))
            .collect();
        Ok(ActiveSessionSnapshot {
            meta,
            binding,
            frozen,
            inherited,
            payloads,
            flags,
        })
    }

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

    /// 有界读取一页历史（向「更早」方向）：**包含已排除的归档条目**。
    ///
    /// - `cursor = None`：首屏，从最新一行开始；`Some` 时按游标锚点继续向更早读。
    /// - `limit`：本页最多推进的行数（不可展示的行同样占位）。
    /// - `byte_budget`：页内条目编码字节之和的上限；单条超预算时独占一页并带
    ///   [`crate::history_page::HistoryPageItem::oversized`] 标记。
    ///
    /// 读取分两阶段（见 [`crate::history_page::HistoryPagePlan`]）：阶段一只读 id + 存储序键
    /// + 正文长度（最多 `limit + 1` 行，不取正文），阶段二只取本页那些行的正文。多读的
    /// 那一条候选行只回答「更早方向还有没有内容」，它的正文**永远不得进内存**（REQ-H03：
    /// 不得先批量物化再裁页）。
    ///
    /// 语义（与存储层实现同源，见 [`crate::history_page::HistoryPagePlan::plan`]）：
    ///
    /// - 不过滤 `excluded`：归档区正是分页要读的东西。
    /// - System 条目与 Model-only reminder 不返回，但位置照常推进；是否还有更早内容按
    ///   **未过滤**的行数判定，因此连续的不可展示条目不会假造终点。
    /// - 游标不可用（形式错误、跨会话、锚点或浏览范围起点已不存在）必须失败，不得回退成
    ///   首屏，也不得用空页掩盖。
    ///
    /// 默认实现失败（`Unsupported`）：空页在契约里是「更早方向没有内容了」的合法含义，
    /// 没实现这条读取的后端不能拿它冒充。
    async fn load_history_page(
        &self,
        _id: &ThreadId,
        _cursor: Option<&HistoryCursor>,
        _limit: usize,
        _byte_budget: usize,
    ) -> SessionResourceResult<HistoryPage> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 有界读取**一段 canonical 完整历史**（向前方向）：未协商 `peri.historyPagination` 的
    /// 标准客户端完整回放的读取入口（需求 §5.3）。
    ///
    /// 与 [`Self::load_history_page`]（向「更早」翻页、读归档区）是两条读取，方向与过滤规则
    /// 都不同：这条读的是**完整历史**（继承区 + 全部自有行，含 Full 归档原文），核心不必
    /// 重新持有整份历史，出口按段流式投影。
    ///
    /// - `cursor = None`：本次回放的**首段**。返回**全部继承区 payload**（来自单行
    ///   `inherited_context` 快照；继承区在本切片不分页）加上自有行中 `rowid` 最小的若干行；
    ///   同一次读取里把 `tail` 定为该线程自有行当前最大 `rowid`，作为本次回放的固定上界。
    /// - `cursor = Some`：从 `after` 之后继续（`rowid > after`），且**不得越过 `tail`**
    ///   （判定用 `rowid <= tail`；不要求 `tail` 那一行仍存在——上界是一个数值）。
    ///   已知边界（本契约不保证，也不加校验）：`tail` 那一行被删（rewind 的尾部切除）之后，
    ///   SQLite 会把它的 `rowid` 复用给新插入的行，这类新行因此落在 `(after, tail]` 内被本次
    ///   回放读到，而被删行的内容静默消失；回放游标没有结构版本可比（`.recon/S9-boundedness.md`
    ///   第 3.4 节第 6 点与第 6 节 R1 的实测）。
    /// - `limit`：本段最多推进的自有行数；`byte_budget`：段内正文编码字节之和的上限。
    ///   单条超预算时独占一段并**完整交付正文**（完整回放不允许静默丢内容）。读取分两阶段：
    ///   阶段一最多取 `limit + 1` 行，只回答「id + 存储序键 + 正文长度」（多读的那一条用来判断
    ///   「还有没有更多」）；阶段二只取本段那些行的正文。阶段一的正文**不越过 SQL 边界、不进
    ///   解码器**（`SELECT` 列表里没有正文），但长度表达式 `length(CAST(content AS BLOB))` 会让
    ///   SQLite 关掉 length-arg 优化，引擎仍可能把候选行的正文读进寄存器——包括那一条永不被
    ///   返回的 lookahead 行（代价是每段多一次 I/O 与瞬时内存，不改变返回集与解码字节；
    ///   实测见 `.recon/S9-boundedness.md` 第 3.4 节第 3 点与第 6 节 R2）。
    /// - **不过滤 `excluded`，也不过 System / model-only reminder**：这是完整回放，可展示性
    ///   过滤属于调用方（ACP 投影）。
    /// - 锚点消失**失败关闭**：`after` 指定的行已不在存储里即失败（不得回退成从头开始，也
    ///   不得用空段冒充结束；与 [`Self::load_history_page`] 的游标过期同一处置）。首段的
    ///   `tail` 与首段返回的行同出于一次一致读取；此后各段只按 `rowid` 区间读取，**不**校验
    ///   行集合仍与首段同源（见上面的 rowid 复用边界）。
    /// - `next_cursor = None` **只**表示「没有更多自有行了」（已到 `tail`，或本来就没有行）。
    ///
    /// 默认实现失败（`Unsupported`）：空段在契约里是「回放结束」的合法含义，没实现这条读取
    /// 的后端不能拿它冒充——调用方据此**显式**回退到 [`Self::load_session_history`] 的全量
    /// 读取，或让本次读取失败，两条路都要看得见。
    async fn load_history_segment(
        &self,
        _id: &ThreadId,
        _cursor: Option<&HistoryForwardCursor>,
        _limit: usize,
        _byte_budget: usize,
    ) -> SessionResourceResult<HistorySegment> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 有界读取浏览范围边界（一次单行查询）：该会话自有历史最旧一条 + 范围身份。
    ///
    /// 与 [`Self::load_history_page`] 的首屏边界**同一份事实**（同一条查询的同一形状），
    /// 供初始 load 判断「更早方向还有没有内容」（没有就是 `nextCursor = null`）并铸造第一个
    /// 游标，见 [`HistoryPageBounds`]。它不得退化成扫描线程或聚合：调用发生在初始 load 上。
    ///
    /// 默认实现失败（`Unsupported`）：没有这条读取的后端给不出「有没有更早内容」的事实，
    /// 也不该拿一个猜出来的起点或空范围冒充一个正常回答。
    async fn history_page_bounds(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<HistoryPageBounds> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 自有**有效**条目的存储序键（`position` 的事实源）：`message_id → 存储序键`。
    ///
    /// 用途：初始 load 的回放要把每条自有条目打上**与页可比**的位置（REQ-H01-3、REQ §5.2 的
    /// `entryId` / `position`），客户端才能把后到的页并进稀疏首屏的正确区间。只读 id 与序键，
    /// **不读正文**；范围与 [`Self::load_active_snapshot`] 的自有区同一读取条件
    /// （`excluded = 0`），因此只回答「这一条自有有效条目在哪」，不做全历史扫描。
    ///
    /// 继承区条目**不在**其中（它们在本线程没有行）：调用方必须把缺席解释为「不是本线程的
    /// 自有条目」，不得当成默认位置或 0——那会把继承条目排到自有条目之间。
    ///
    /// 默认实现返回空 map（非生产后端的回退）；生产后端必须覆写。未覆写的后端给不出位置，
    /// 回放只打 `entryId` 不打位置戳：这是**能力缺失**，客户端不得把它读成「没有更早历史」。
    async fn load_entry_positions(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<HashMap<MessageId, u64>> {
        Ok(HashMap::new())
    }

    /// 按给定 id 集合读取 flags，范围有界：只回答这些 id 的标记，不做全历史扫描。
    ///
    /// 默认实现返回空 map（非生产后端的回退，不满足分页路径的读取范围要求；
    /// 生产后端应覆写为按 id 集合的有界查询）。调用方必须**保守**解释缺席：
    /// 不在返回 map 里的 id 是「未知」，不是「默认标记」——把缺席当默认会把没读到
    /// 的排除/投影事实误判成未标记。
    async fn load_message_flags(
        &self,
        _id: &ThreadId,
        _ids: &[MessageId],
    ) -> SessionResourceResult<HashMap<MessageId, MessageFlags>> {
        Ok(HashMap::new())
    }

    /// 小型 metadata 投影；不加载历史或大快照。
    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta>;
    async fn session_environment_id(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        Ok(None)
    }
    async fn session_workspace_id(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Option<crate::workspace::WorkspaceId>> {
        Ok(None)
    }

    async fn list_machines(&self) -> SessionResourceResult<Vec<crate::workspace::MachineInfo>> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    async fn list_workspaces(
        &self,
        _machine_id: &str,
    ) -> SessionResourceResult<Vec<crate::workspace::WorkspaceInfo>> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    async fn rename_machine(&self, _machine_id: &str, _name: &str) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 按 scope/cursor/limit 分页列举；过滤在数据端完成。
    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage>;

    async fn list_archived_sessions(
        &self,
        _query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 将根 Session 移入或移出归档；不会改动历史与活动时间。
    async fn set_session_archived(
        &self,
        _id: &ThreadId,
        _archived: bool,
    ) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

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

    /// 按稳定消息 ID 原子落下终态提醒；同一内容的重放只返回 false。
    /// 同 ID 的不同内容必须失败，不可把它当作已交付。
    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool>;

    /// 接纳显式关闭；提交后恢复的 runtime 必须禁止新任务准入。
    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 读取显式关闭意图；Agent 意外断连不产生该事实。
    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool>;

    /// 保存 fork 目标快照；source 不变。
    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()>;

    /// 保存 child：继承区与父子关系一起成立。
    ///
    /// 父子关系在快照里出现两次（[`ChildSnapshot::parent_id`] 与
    /// [`NewSessionMeta::parent_thread_id`]），实现必须在写入前要求两者一致；不一致、
    /// 自指父关系、把自己当根都必须拒绝，且拒绝不留任何写入（详见 `ChildSnapshot` 不变量）。
    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()>;

    /// 在 root 执行树内串行认领 child resume。
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

    /// 按显式边界 rewind，并在**同一次提交**里重建该边界的有效视图。
    ///
    /// `clear_excluded` 是保留前缀里必须清零 `excluded` 的 id，由调用方从一致读取的完整
    /// own 历史算出（[`crate::store::history::rewind_clear_excluded_ids`] 是唯一规则实现）：
    /// 边界删除与 flags 清零要么都生效、要么都不生效，只生效一半会让热态与随后的冷恢复
    /// 看到两份不同的历史。清零只改 `excluded`，`truncated`/`projection` 保持原样。
    ///
    /// 未知边界保持「无变更」语义，但**必须如实回报** [`RewindOutcome::BoundaryMissing`]：
    /// 调用方（如 ACP rewind handler）不能把「什么都没做」当成「已按边界重建」，
    /// 否则热态会被改写成存储里不存在的那份。transcript 同步路径（KeepThrough）是
    /// 唯一按「缺失即无操作」消费该结果的调用方，理由见其调用点注释。
    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome>;

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
