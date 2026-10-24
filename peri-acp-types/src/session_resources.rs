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
    HistoryCursor, HistoryForwardCursor, HistoryPage, HistoryPageBounds, HistorySegment,
};
use crate::history_window::{
    HistoryBodyChunk, HistoryCommittedTail, HistoryReadTicket, HistorySource,
};
use crate::messages::MessageId;
use crate::store::{CompactionChange, InheritedContext, MessageFlags, PersistedPayload};
use crate::system_reminder::TrustedSystemReminder;
use crate::thread::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta};
use crate::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding};

// 历史读取视图与 rewind 契约类型按职责拆入子模块 `history`（见该模块文档）；
// `pub use` 重导出保持既有公开路径 `crate::session_resources::X` 可用。
pub mod history;

// `peri.historyWindowV1` 的存储端口类型：边界、双向计划与有界结果。
pub mod history_window;
pub use history_window::{
    HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowRows,
    HistoryWindowTailExpectation, InheritedHistoryEntry, SessionHistoryWindowBounds,
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

/// 绑定复核的力度。
///
/// 文件对象身份与完整发现快照不再参与准入，因此两种力度现在执行同一套检查
/// （归属关系与执行目录可用性）；`Full` 保留为一次准入入口的既有标记，避免
/// 协议与调用方改动。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingRecheck {
    /// 一次准入入口的权威复核（当前与 `Recorded` 等价）。
    Full,
    /// 同一次准入内的复核（当前与 `Full` 等价）。
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

// rewind 与有界读取视图的类型本体（本交付新增）在同名子模块 `history`。
pub use history::{
    collect_rewind_candidates, rewind_candidate, rewind_candidate_text, ActiveSessionSnapshot,
    RewindBoundary, RewindCandidate, RewindOutcome,
};

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

mod error;

pub use error::{
    CloseSettlement, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

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
#[allow(clippy::too_many_arguments)] // 历史窗口读取参数为跨层固定契约，逐项显式传递，不包结构体
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

    /// 接纳已确认的本机 legacy 会话：补上缺失的绑定，frozen 缺省时一并定格。
    ///
    /// 竞争时返回胜者事实，不返回 CAS 布尔值；`saved_cwd` 必须是会话保存的原始
    /// 路径（不允许调用方借接纳顺手改绑）。既有 frozen 字节是唯一事实源：只补空列、
    /// 不因候选覆盖（write-once），也就没有「已认领过」与「从未认领」的区别。
    /// 子会话的绑定身份必须与父会话逐字一致（父绑定尚未成立时如实拒绝）。
    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()>;

    /// 为无绑定的本机历史会话重建可复核的执行归属：幂等、可重跑。
    ///
    /// 后置条件：会话已有绑定时只按既有事实复核；没有绑定时按保存的 cwd 在本机重新
    /// 观测，原归属行从未被观测过（无执行证据的迁移期历史归组行）才把会话归属收敛到
    /// 观测出的 Workspace，行上已有证据则失败（
    /// [`WorkspaceError::UnverifiedLegacyOwner`]），不静默改绑。
    /// 返回的 Workspace 与 `threads.workspace_id` 一致，接纳可直接使用它；
    /// 无法用当前证据证明归属时如实失败，历史仍按 ID 可读。
    async fn relink_unbound_session(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    // ── 读取 ──

    /// 一次一致读取：metadata、binding 分类、frozen 状态、own payload/flags、inherited。
    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot>;

    /// Explicit independent-fork read: flatten the frozen inherited context and own
    /// history, and materialize only archives authorized by those frozen roots.
    /// Added archive bodies are recovery_only and stay excluded through rewind/refork.
    /// Preserve the frozen source tail and verify catalog/member identities before returning.
    async fn load_fork_snapshot(&self, _id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

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

    /// 轻量读取恢复所需的 frozen 原字节，不分页、不装入任何正文。
    ///
    /// 恢复准入只需要 frozen 这一项不可变事实；默认实现拒绝，生产后端必须覆写为
    /// 单列读取，**不得**用 [`Self::load_active_snapshot`] 作为捷径（那会把 own 有效
    /// payload、flags 与 inherited 一并读入内存）。会话不存在是 `NotFound`，行存在但
    /// 从未保存 frozen 是 [`FrozenState::LegacyAbsent`]。
    async fn load_frozen_state(&self, _id: &ThreadId) -> SessionResourceResult<FrozenState> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 后端是否实现 `peri.historyWindowV1` 的边界/页/正文读取端口。
    ///
    /// 协商必须反映**端到端**可用：客户端声明不能把未实现的后端变成可用。默认 false；
    /// 覆写了窗口读取端口的后端必须同时覆写本方法。
    fn supports_history_window(&self) -> bool {
        false
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

    /// 有界读取会话级回退候选：只返回 `Human` 且剥离 `system-reminder` 后仍有用户文本的
    /// 条目，最新在前，最多 `limit` 条。
    ///
    /// - **不过滤 `excluded`**：被 Full Compact 归档的 user 消息仍是合法回退目标。
    /// - 返回的 `text` 是原始正文，预览清洗（剥离注入块、200 字符截断）留给 ACP。
    /// - 实现**不得**为了取出 user 行而解码 Ai/Tool 正文；生产后端必须用读取条件把
    ///   非 user 行挡在解码之前。
    /// - 继承区 Human 条目仍可出现（与 [`Self::load_session_history`] 的逆序口径一致），
    ///   排在自有条目之后。
    ///
    /// 默认实现回退到 [`Self::load_session_history`] 后过滤：语义正确但会读入全部正文，
    /// 只供测试替身或尚未实现有界读取的后端使用；生产后端必须覆写为有界读取。
    async fn load_rewind_candidates(
        &self,
        id: &ThreadId,
        limit: usize,
    ) -> SessionResourceResult<Vec<RewindCandidate>> {
        let history = self.load_session_history(id).await?;
        Ok(collect_rewind_candidates(&history, limit))
    }

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

    /// 有界读取 `peri.historyWindowV1` 的边界事实：own 最旧/最新稳定身份、durable tail、
    /// 可重建 checkpoint 与冻结前缀状态；**不读任何正文**。
    ///
    /// 默认实现失败（`Unsupported`）：空边界会被出口读成「没有历史」，没有实现这条读取的
    /// 后端不能拿它冒充；`open` 必须显式失败。
    async fn history_window_bounds(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<SessionHistoryWindowBounds> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 只读计划指定的有界 keyset 区间（before/after 双向），返回候选行/指定正文。
    ///
    /// 实现必须：
    ///
    /// - 在 SQL/存储条件里限制区间与固定 tail，不得扫描全历史后在 Rust 过滤；
    /// - 只取本页选中行的正文，lookahead 候选行正文不得出库；
    /// - `excluded` 归档区可按明确区间读取，但不得把它放回有效上下文；
    /// - tail 身份/位置不匹配时返回 [`crate::history_window::HistoryWindowError`]，
    ///   不得降级成空页。
    ///
    /// 默认实现失败（`Unsupported`）：空页在窗口契约里是合法结果，不能用它冒充未实现。
    async fn load_history_window_page(
        &self,
        _id: &ThreadId,
        _plan: &HistoryWindowPagePlan,
    ) -> SessionResourceResult<HistoryWindowRows> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 按稳定条目身份在固定 tail 内重新定位；内部复用双向 keyset 计划。
    ///
    /// 默认实现只把请求转成 [`HistoryWindowPagePlan::seek`] 再调用
    /// [`Self::load_history_window_page`]：后者未实现时同样 fail-closed；实现 page 的
    /// 生产后端不需要再写一份 seek 读取。
    async fn seek_history_window(
        &self,
        id: &ThreadId,
        source: HistorySource,
        anchor: MessageId,
        before_limit: usize,
        after_limit: usize,
        byte_budget: usize,
        committed_tail: &HistoryCommittedTail,
    ) -> SessionResourceResult<HistoryWindowRows> {
        let plan = HistoryWindowPagePlan::seek(
            source,
            anchor,
            before_limit,
            after_limit,
            byte_budget,
            committed_tail.clone(),
        );
        self.load_history_window_page(id, &plan).await
    }

    /// 有界读取超大条目的正文块（原始字节）。
    ///
    /// 实现必须使用驱动/存储层的有界读取（如 SQLite `substr`），不得先取出完整正文再
    /// 在 Rust 切片；驱动无法保证有界块时返回 `Unsupported`，不得退回
    /// [`Self::load_session_history`]。
    ///
    /// `committed_tail` 是 open 时固定的持久尾界：存储层必须拒绝 `rowid/position` 超出该
    /// 尾界的正文行，不得只按 `message_id` 读取。
    ///
    /// `read_ticket` 对 adapter 是**不透明上下文**：它不构成访问授权，Host 必须在调用前
    /// 完成票据/会话/删除状态校验。adapter 不得记录或解释票据内容。
    ///
    /// 默认实现失败（`Unsupported`）。
    async fn load_history_body_chunk(
        &self,
        _id: &ThreadId,
        _source: HistorySource,
        _entry_id: MessageId,
        _offset: u64,
        _max_bytes: u32,
        _committed_tail: Option<&HistoryCommittedTail>,
        _read_ticket: &HistoryReadTicket,
    ) -> SessionResourceResult<HistoryBodyChunk> {
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

    /// Resolve a frozen inherited archive grant. Source IDs/references are never credentials.
    async fn inherited_archive_root(
        &self,
        _id: &ThreadId,
        _source: &ThreadId,
        _anchor: MessageId,
    ) -> SessionResourceResult<crate::context_archive::ArchiveRoot> {
        Err(crate::session_resources::SessionResourceError::new(
            crate::session_resources::SessionResourceErrorKind::Unsupported,
        ))
    }

    /// Bounded archive metadata access, including excluded anchors. Unsupported adapters fail closed.
    async fn read_archive_metadata(
        &self,
        _id: &ThreadId,
        _query: &crate::context_archive::ArchiveMetadataQuery,
    ) -> SessionResourceResult<crate::context_archive::ArchiveMetadataPage> {
        Err(crate::session_resources::SessionResourceError::new(
            crate::session_resources::SessionResourceErrorKind::Unsupported,
        ))
    }

    /// Read only the requested canonical byte interval below a stable committed tail.
    async fn read_archive_chunk(
        &self,
        _id: &ThreadId,
        _query: &crate::context_archive::ArchiveChunkQuery,
    ) -> SessionResourceResult<crate::context_archive::ArchiveChunk> {
        Err(crate::session_resources::SessionResourceError::new(
            crate::session_resources::SessionResourceErrorKind::Unsupported,
        ))
    }

    /// Read a bounded batch in request order. Adapters may combine it into one I/O.
    async fn read_archive_chunks(
        &self,
        id: &ThreadId,
        batch: &crate::context_archive::ArchiveChunkBatch,
    ) -> SessionResourceResult<Vec<crate::context_archive::ArchiveChunk>> {
        let mut chunks = Vec::with_capacity(batch.queries().len());
        for query in batch.queries() {
            chunks.push(self.read_archive_chunk(id, query).await?);
        }
        Ok(chunks)
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
    /// 看到两份不同的历史。清零同时撤销 `superseded_by`；仍有效的 archive catalog
    /// 保留身份，丢失锚点、成员或替代依据的整组省略动作与入口在同一事务撤销。
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

mod lifecycle;
pub use lifecycle::SessionStoreShutdownPort;

#[cfg(test)]
#[path = "session_resources_test.rs"]
mod tests;
