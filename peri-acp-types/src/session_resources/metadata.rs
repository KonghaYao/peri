//! 会话资源的能力、创建输入与元数据契约；公共路径由父模块重导出。

use crate::messages::MessageId;
use crate::store::{InheritedContext, MessageFlags, PersistedPayload};
use crate::thread::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta};
use crate::workspace::SessionBinding;
use std::collections::HashMap;

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
/// 清理判据见 [`super::SessionResources::discard_incomplete_initialization`]。
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
pub use super::history::{
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
/// [`super::SessionResourceErrorKind`] 承载（原因是原因，效果是效果）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOutcome {
    /// 已确认未生效。
    NotApplied,
    /// 已确认生效（例如数据已保存、执行准入失败）。
    Applied,
    /// 无法证明生效与否：热态必须失效并阻塞续写与 clean。
    Unknown,
}
