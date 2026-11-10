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
use crate::session::{
    MqCancelOutcome, MqEnqueueOutcome, MqEntry, MqFilter, MqHeader, MqInput, MqReceiveOutcome,
    MqReceiveSelection, MqWatermark,
};
use crate::store::{CompactionChange, MessageFlags, PersistedPayload};
use crate::system_reminder::TrustedSystemReminder;
use crate::thread::{AgentStatus, ThreadId, ThreadMeta, WorkState};
use crate::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};

pub mod history;

pub mod history_window;
pub use history_window::{
    HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowRows,
    HistoryWindowTailExpectation, InheritedHistoryEntry, SessionHistoryWindowBounds,
};

mod metadata;
pub use metadata::*;

mod error;

pub use error::{
    CloseSettlement, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

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

    /// 认领终止并提交领域终态，返回本次是否仍拥有该 child 的写入权。
    ///
    /// 具备原子归属能力的 adapter 覆盖本方法，把「恢复到认领前」与终态写入放在
    /// 同一个归属校验内完成：返回 `Ok(false)` 表示新认领已接管该 child，调用方
    /// 不得再写终态。默认实现只有恢复能力（不写终态），返回值恒为 `true`，
    /// 保持既有实现的行为不变。
    async fn finish_with_status(&self, _status: AgentStatus) -> SessionResourceResult<bool> {
        self.mark_terminated().await?;
        Ok(true)
    }
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

    /// 进入会话时的恢复范围：沿已有父链解析根，再读取该根子树里仍为 `working`
    /// 的 thread（设计 §3）。
    ///
    /// 只读：不写工作意图、不建收件地址、不授权启动。默认实现返回 `Unsupported`，
    /// 让非生产后端显式失败，而不是把「不支持」当成「没有候选」。
    async fn load_recovery_scope(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<crate::recovery::RecoveryScope> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 进入根时的遗留 Active 收敛候选：该根子树中 `agent_status = active` 且
    /// 工作意图为 `idle`/`stopped` 的非根 thread（只读）。
    ///
    /// 候选只描述存储事实；本进程活实例与 close 未结清由 Host 侧排除。
    /// 默认实现返回 `Unsupported`，让非生产后端显式失败而不是把「不支持」当成
    /// 「没有需要收敛的 child」。
    async fn load_interrupted_threads(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Vec<crate::recovery::InterruptedThread>> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }
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

    /// 写入 durable 工作意图（`threads.work_state`）。
    ///
    /// 只写这一列：它表达中断后的续跑意图，不承担运行态与列表排序。默认实现返回
    /// `Unsupported`——后端要么如实持久化停止/续跑意图，要么显式拒绝。
    async fn set_work_state(&self, _id: &ThreadId, _state: WorkState) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// turn 收尾把工作意图收敛到 `idle`，但不得覆盖仍要求继续处理的新工作。
    ///
    /// 与消息接纳共用协调：存储操作内确认没有待处理的 `EnsureProcessing` 条目后
    /// 才写 `idle`；`stopped` 不被收尾解除。返回是否真的写入了 `idle`。
    ///
    /// 默认实现退回 [`Self::set_work_state`]（无条件写 `idle`）：没有事务能力的
    /// 后端只能保证「没有新工作时写 idle」，不能凭空判定待处理消息。
    async fn settle_work_state_after_turn(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.set_work_state(id, WorkState::Idle).await?;
        Ok(true)
    }

    /// 写入最近一次有效上下文占用快照（`threads.context_usage_tokens`）。
    ///
    /// `None` 表示「未知」，写 NULL；不更新 `updated_at`。默认实现返回 `Unsupported`。
    async fn set_context_usage(
        &self,
        _id: &ThreadId,
        _tokens: Option<u64>,
    ) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

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

    // ── Thread MQ（持久收件箱） ──
    //
    // 五个方法构成一个行为面：`thread_mq` 只保存「尚未移交」的消息，移交与取消都物理
    // 删除行，不保存终态、墓碑或消费留档。默认实现一律返回 `Unsupported`——后端要么
    // 支持原子移交，要么显式拒绝；回退成「先写历史再删 MQ」的两次独立写入不允许
    // （见 `docs/design/thread-mq.md` §4.2）。

    /// 接纳一条入站消息到目标 thread 的 MQ。
    ///
    /// 同一 ID 的既有 MQ 行内容一致时复用原接纳结果（含原入队时间）；同一 ID 已在历史
    /// 返回 [`MqEnqueueDisposition::AlreadyHandedOff`]；内容冲突或 ID 属于其他 thread
    /// 返回 [`SessionResourceErrorKind::Conflict`]，不覆盖旧行也不改路由。
    /// `enqueued_at` 由存储端生成，重试不得改写。
    async fn mq_enqueue(
        &self,
        _thread_id: &ThreadId,
        _input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 按 `(enqueued_at, id)` 升序读取本 thread 的 MQ 行；只读，不消费。
    async fn mq_query(
        &self,
        _thread_id: &ThreadId,
        _filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// MQ 控制面投影：与 [`Self::mq_query`] 同过滤语义，但不解码正文。
    ///
    /// 默认实现退回完整查询；具备列投影能力的后端应覆盖，避免布尔/策略判定
    /// 被正文大小放大。
    async fn mq_headers(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqHeader>> {
        Ok(self
            .mq_query(thread_id, filter)
            .await?
            .into_iter()
            .map(|entry| entry.header())
            .collect())
    }

    /// 本 thread 的 MQ 行数。默认退回完整查询。
    async fn mq_count(&self, thread_id: &ThreadId) -> SessionResourceResult<usize> {
        Ok(self.mq_query(thread_id, MqFilter::default()).await?.len())
    }

    /// 本 thread 是否至少有一条 MQ 行。默认退回完整查询。
    async fn mq_exists(&self, thread_id: &ThreadId) -> SessionResourceResult<bool> {
        Ok(!self
            .mq_query(thread_id, MqFilter::default())
            .await?
            .is_empty())
    }

    /// 本 thread 的最大 `(enqueued_at, id)`。默认退回完整查询。
    async fn mq_watermark(
        &self,
        thread_id: &ThreadId,
    ) -> SessionResourceResult<Option<MqWatermark>> {
        Ok(self
            .mq_query(thread_id, MqFilter::default())
            .await?
            .last()
            .map(MqEntry::watermark))
    }

    /// 取消一条仍在 MQ 的用户输入，并返回整行原稿。
    ///
    /// 只允许 `source == UserInput`（否则 `InvalidInput`）。并发 Receive 已提交时返回
    /// [`MqCancelOutcome::AlreadyHandedOff`]，两条路径都没有该 ID 时返回
    /// [`MqCancelOutcome::NotFound`]。
    async fn mq_cancel(
        &self,
        _thread_id: &ThreadId,
        _id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 原子移交：同一事务/批次内写历史并删除 MQ 行。
    ///
    /// 逐项按选择顺序处理；MQ 行不存在或 `enqueued_at` 已变是 `Missing`（不写历史）。
    /// 提交成功才返回结果，提交结果未知时调用方须按稳定 ID 重查，不得当作成功。
    async fn mq_receive(
        &self,
        _thread_id: &ThreadId,
        _selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 清空本 thread 的全部 MQ 行；只给 rewind 与 thread 生命周期调用。
    async fn mq_clear(&self, _thread_id: &ThreadId) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 按稳定消息 ID 查询历史事实：该 ID 是否已作为**本 thread** 的消息持久存在。
    ///
    /// 只读、有界（单条 ID，不扫描历史）。发送结果必须用它区分「已移交」与「不存在」；
    /// 默认 `Unsupported` 让后端显式报告「无法确认」，调用方不得把它当成不存在。
    async fn mq_history_contains(
        &self,
        _thread_id: &ThreadId,
        _id: MessageId,
    ) -> SessionResourceResult<bool> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 本地 child 终态结算：终态、工作意图与结果消息的 MQ 接纳一次提交。
    ///
    /// 语义要求：三条事实都生效才返回成功。幂等判据是「这次结算是否已落地」：child
    /// 已是终态 **且** 结果消息的稳定 ID 已在目标 thread 的 MQ 或历史里时，重复结算
    /// 必须返回成功的 no-op（child 状态、`work_state` 与目标 MQ 都不变）；同 ID 但尚未
    /// 落地的重试继续走写入路径。`target` 必须与 `child` 同属一棵会话树（通常是父会话，
    /// 恢复期可能是 child 自己）。消息以稳定 ID 幂等接纳，重复提交得到
    /// `Existing`/`AlreadyHandedOff`。
    ///
    /// 后端必须覆写为单事务/单批；没有原子结算能力时返回 `Unsupported`，不写部分事实。
    async fn finalize_child_terminal(
        &self,
        _child: &ThreadId,
        _status: AgentStatus,
        _work_state: WorkState,
        _target: &ThreadId,
        _message: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 无结果终态结算：仅从 active 原子写入终态及工作意图；已有终态不改写。
    /// 后端未实现原子操作时返回 `Unsupported`，不进行逐步写入。
    async fn settle_terminal_without_result(
        &self,
        _child: &ThreadId,
        _status: AgentStatus,
        _work_state: WorkState,
    ) -> SessionResourceResult<TerminalSettlementOutcome> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 关闭结算：保留提醒写入与 MQ 快照水位删除在同一次提交中完成。
    /// 提醒按稳定 ID 幂等；已不存在或水位变化的 MQ 行不删除。
    /// 后端未实现原子操作时返回 `Unsupported`，不进行逐步写入。
    async fn settle_retained_mq(
        &self,
        _thread_id: &ThreadId,
        _reminders: &[crate::session::RetainedReminder],
        _rows: &[MqEntry],
    ) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    /// 仅测试：让下一次原子移交在写入前暂停；默认实现忽略门控。
    ///
    /// 必须经适配器转发到真正的存储实现，否则 `MessageQueue::gate_next_receive`
    /// 在绑定资源的路径上静默失效。
    #[cfg(feature = "test-support")]
    fn install_receive_gate(&self, _gate: crate::session::ReceiveGate) {}

    /// 仅测试：让下一次 MQ 查询在读取前暂停；默认实现忽略门控。
    ///
    /// 必须经适配器转发到真正的存储实现，否则 `MessageQueue::gate_next_query`
    /// 在绑定资源的路径上静默失效。
    #[cfg(feature = "test-support")]
    fn install_query_gate(&self, _gate: crate::session::ReceiveGate) {}
}

mod lifecycle;
pub use lifecycle::SessionStoreShutdownPort;

mod settlement;
pub use settlement::TerminalSettlementOutcome;

#[cfg(test)]
#[path = "session_resources_test.rs"]
mod tests;
