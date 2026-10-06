//! 数据端口 — 资源模块内部行为 seam，由两个 adapter（本机 SQLite / Turso Cloud）实现。
//!
//! 边界规则：
//!
//! - 只暴露会话行为，**不**暴露 SQL、事务、CAS、隔离级别、连接、SQL batch、底层
//!   重试令牌或补偿协议；「整体生效或整体不生效」是行为后置条件，由 adapter 自行
//!   选择机制实现。
//! - 业务侧拿不到本 trait：门面（`SessionResourcesImpl`）是唯一调用方，也是唯一注入点。
use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, NewSession,
    PersistenceRecovery, RewindBoundary, SessionMetaPatch, SessionResourceResult, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding,
};

use super::failure::invalid_input;

/// child resume 认领的持久化事实：状态 + 是否处于认领中。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildResumeRecord {
    pub status: peri_acp_types::thread::AgentStatus,
    pub claimed: bool,
}

/// 会话数据行为端口。
///
/// 每个 mutation 的领域后置条件与门面一致；adapter 可以自由选择实现机制
/// （SQLite 同库塌缩为一个事务、远程 adapter 见其自身原子保证），但不得把
/// 「未生效」报告成成功，也不得在失败后遗留部分写入。
#[async_trait]
pub(crate) trait SessionDataPort: Send + Sync {
    async fn load_work_delivery(
        &self,
        query: &peri_acp_types::session_resources::work::WorkDeliveryQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::DeliveryRecord>>;
    async fn has_pending_work_mutations(&self, id: &ThreadId) -> SessionResourceResult<bool>;
    async fn load_work_command(
        &self,
        query: &peri_acp_types::session_resources::work::WorkCommandQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::OwnedWorkCommand>>;
    async fn load_session_work(
        &self,
        query: &peri_acp_types::session_resources::work::WorkQuery,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkSnapshot>;
    async fn apply_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::WorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkReceipt>;
    async fn resolve_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::WorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkResolution>;
    async fn load_session_control(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlState>;
    async fn apply_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlReceipt>;
    async fn resolve_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlResolution>;
    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()>;
    async fn close_settlement(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::CloseSettlement>;

    fn oauth_credentials_for_workspace(
        self: std::sync::Arc<Self>,
        _workspace_id: peri_acp_types::workspace::WorkspaceId,
    ) -> Option<std::sync::Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        None
    }
    async fn machine_id_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>>;
    async fn workspace_id_of(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::workspace::WorkspaceId>> {
        Ok(None)
    }
    async fn list_machines(
        &self,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::MachineInfo>> {
        Err(
            peri_acp_types::session_resources::SessionResourceError::new(
                peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
            ),
        )
    }
    async fn list_workspaces(
        &self,
        _machine_id: &str,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::WorkspaceInfo>> {
        Err(
            peri_acp_types::session_resources::SessionResourceError::new(
                peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
            ),
        )
    }
    async fn rename_machine(&self, _machine_id: &str, _name: &str) -> SessionResourceResult<()> {
        Err(
            peri_acp_types::session_resources::SessionResourceError::new(
                peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
            ),
        )
    }
    /// 保存新会话：meta/binding/frozen 在同一事务完整落库。
    async fn save_new_session(&self, input: &NewSession) -> SessionResourceResult<()>;
    async fn save_new_session_in_workspace(
        &self,
        input: &NewSession,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.save_new_session(input).await
    }

    /// 保存未发布创建（J2 第一阶段）：身份/绑定落库，frozen 暂空。
    async fn save_new_session_draft(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<()>;
    async fn save_new_session_draft_in_workspace(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.save_new_session_draft(draft).await
    }

    /// 一次性提交 frozen（write-once CAS）：仅当该 identity 从未提交过时生效。
    ///
    /// 两种存储后端都在事务内提交；重复提交返回 typed 冲突而不是覆盖。
    async fn commit_frozen(
        &self,
        id: &ThreadId,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()>;

    /// 撤销未发布的创建：只针对本次初始化，不修改既有 source 会话。
    ///
    /// 语义由**入口**决定，不由数据判据决定：本方法是 write-once 完整创建（fork/一次创建）
    /// 的失败补偿入口——目标创建即带 frozen，撤销就是把它整条删掉（与 source 无关，可由
    /// source 重生成）。两阶段草稿的撤销走 [`Self::revoke_unpublished_draft`]。
    async fn revoke_unpublished_session(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 撤销**两阶段草稿**（`frozen` 尚未提交的未发布创建）：
    /// [`SessionInitialization`](peri_acp_types::session_resources::SessionInitialization::abandon)
    /// 驱动的补偿入口。
    ///
    /// 与 [`Self::revoke_unpublished_session`] 的差别是唯一的一条判据：`frozen IS NULL`。
    /// 已提交 frozen 的草稿是「已定稿、未发布」的合法中间态，删除它会销毁内容准入的成果
    /// 因此必须返回 typed 冲突且一条行都不删。
    async fn revoke_unpublished_draft(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 接纳 legacy 会话：binding 与缺失的 frozen 一次成立，已有值不变。
    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()>;

    /// 一致读取：meta/binding 分类/frozen 状态/own payload+flags/inherited。
    async fn load_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot>;

    /// 轻量绑定分类（不加载历史）：记录事实三种——绑定与本机登记一致、绑定指向的登记
    /// 已不存在（含子会话没有绑定行）、无绑定行。`LegacyConfirmed` 由门面按本机来源
    /// 证据联合判定，不在这里冒充。
    async fn load_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState>;

    /// 完整逻辑上下文：继承区在前、自有 payload 在后。
    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>>;

    /// 小型 metadata 投影（不加载历史或大快照）。
    async fn load_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta>;

    /// 会话数据是否存在（不含终态判定）。
    async fn session_exists(&self, id: &ThreadId) -> SessionResourceResult<bool>;

    /// 已保存的绑定字节；没有绑定行时 `None`。
    ///
    /// 绑定字节是**数据事实**：本机 adapter 从 `session_bindings` 读，远端 adapter 从远端
    /// 会话行自带的 `binding_*` 列读。执行面只按调用方给出的字节做本机目录复核。
    async fn binding_of(&self, id: &ThreadId) -> SessionResourceResult<Option<SessionBinding>>;
    /// Per-session immutable execution evidence. `None` denies execution;
    /// history remains readable by Session ID.
    async fn binding_discovery_snapshot(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        Ok(None)
    }
    /// 该会话在树中的根（含自身）。
    ///
    /// 父链是数据事实：未结清判定按整棵树聚合时，远端会话在本机没有 `threads` 行，
    /// 因此「这条会话属于哪棵树」只能由持有 canonical 父链的一侧回答。
    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId>;

    /// 分页列举：过滤在数据端完成。
    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage>;

    async fn list_archived_sessions(
        &self,
        _query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        Err(
            peri_acp_types::session_resources::SessionResourceError::new(
                peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
            ),
        )
    }

    async fn set_session_archived(
        &self,
        _id: &ThreadId,
        _archived: bool,
    ) -> SessionResourceResult<()> {
        Err(
            peri_acp_types::session_resources::SessionResourceError::new(
                peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
            ),
        )
    }

    /// 直接子会话 metadata。
    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;

    /// 以 `root` 为根的整棵树 metadata（含自身）。
    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;

    /// 追加 canonical payload 批次：顺序稳定、计数与自动标题一致维护，
    /// id 冲突（已存在或批次内重复）必须失败，不得静默忽略。
    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()>;

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool>;

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()>;
    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool>;

    /// 保存 fork 目标快照；source 不变。
    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()>;
    async fn save_fork_in_workspace(
        &self,
        fork: &ForkSnapshot,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.save_fork(fork).await
    }

    /// 保存 child：继承区与父子关系一起成立。
    ///
    /// 调用前必须先过 [`ensure_child_relation`]：父子关系在快照里出现两次（`parent_id` 与
    /// `target.meta.parent_thread_id`），不一致时落库的事实会与声明的相反。
    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()>;

    /// 读取 child resume 认领事实。
    async fn load_child_resume_record(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<ChildResumeRecord>;

    /// 写入 child resume 认领事实（active 标记与终态由门面按领域结果给出）。
    async fn store_child_resume_record(
        &self,
        child: &ThreadId,
        record: &ChildResumeRecord,
    ) -> SessionResourceResult<()>;

    /// 应用一次 compaction 变更：flags、摘要追加、派生计数与缓存视图全部生效或全不生效。
    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()>;

    /// 应用投影/flags 变更集，并由数据端同步维护派生缓存视图。
    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()>;

    /// 按显式边界 rewind。
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

    /// 定向 metadata 更新。
    async fn update_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()>;

    /// 删除会话树：删除即删除，数据删除之后本机与远端都不再持有该会话的终态证据。
    async fn delete_tree(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 收敛未决持久化，返回是否已可重载。
    ///
    /// 本机实现在同事务内完成写入，因此没有需要收敛的中间态；远程实现的收敛由远端
    /// adapter 内部完成（见其模块文档）。本机**没有**跨进程的未决记录：那类锚点已被
    /// 移除（用户裁决不做跨安装能力），进程内门禁保留未决写入效果。
    async fn recover_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery>;

    /// 排空该会话已排队的持久化写入（有界等待）。
    async fn drain(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 关闭数据端口：返回后不再接受新写入。
    async fn close(&self) -> SessionResourceResult<()>;
}

// ─── child 快照的输入一致性：唯一一条规则 ─────────────────────────────────────

/// child 快照自身的父子/根归属是否自洽——**唯一**一条判定，门面与两个 adapter 共用。
///
/// 落库的父子关系取自 `child.target.meta.parent_thread_id`，而调用方声明的关系在
/// `child.parent_id` / `child.root_id`。两组字段必须指向同一次关系，否则会出现「准入按声明、
/// 落库按另一套」的两种真相：声明了合法父/根、而目标 meta 里没有父的 child 会被写成一条
/// 没有父的**独立 root**，破坏任务树的数据归属。
///
/// | 拒绝理由 | 为什么不能交给别处 |
/// | --- | --- |
/// | `target.meta.parent_thread_id != Some(parent_id)`（含 `None`） | 声明不是权威，落库才是；库内看不出「声明被忽略」 |
/// | `parent_id == target.thread_id` | 自指父关系会形成环 |
/// | `root_id == target.thread_id` | 待创建的新 child 不可能已经是自己的根 |
///
/// 判定不读任何存储、不看「库里有没有会话」，因此门面可以在**任何副作用之前**拒绝
/// （不发门禁、不留未决证据），adapter 也能在连接或事务之前拒绝。
pub(in crate::sessions) fn ensure_child_relation(
    child: &ChildSnapshot,
) -> SessionResourceResult<()> {
    if child.target.meta.parent_thread_id.as_ref() != Some(&child.parent_id) {
        return Err(invalid_input(
            "child snapshot parent relation disagrees with its target meta",
        ));
    }
    if child.parent_id == child.target.thread_id {
        return Err(invalid_input("child session cannot be its own parent"));
    }
    if child.root_id == child.target.thread_id {
        return Err(invalid_input("child session cannot be its own root"));
    }
    Ok(())
}
