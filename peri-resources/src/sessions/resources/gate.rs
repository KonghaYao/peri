//! 写入准入闸门：门面与它发出的认领 handle 共用同一套 mutation 检查。
//!
//! 顺序固定为「能力/权限 → 本 root owner」，检查在门面内部完成，不靠调用方先查。
//! 效果结清只认确定性：`Applied | NotApplied` 才释放准入，`Unknown`（含取消与提交
//! 未确认）把范围留给 `Drop`，由租约留下未决证据。
//!
//! 未决持久化**只在进程内的租约上**表达：v10 移除了本机 durable 锚点（登记、未决写、
//! 远端操作日志），因为用户裁决不做跨安装/跨 store 的能力。文件里因此没有「查表问未决」
//! 这一步——`WriteScope::settle` 的 `Drop` 语义与 `is_uncertain` 读取仍覆盖在途写入；
//! 进程崩溃后的未结清代际由 `execution_runs` 的 `clean = 0` 表达（`Dirty` 分类）。

use std::sync::Arc;

use peri_acp_types::session_resources::{
    MutationOutcome, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::WorkspaceError;

use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::{LocalExecutionPort, SessionFacts};
use crate::sessions::resources::lifecycle::{Lifecycle, LifecycleState};
use crate::sessions::sqlite_store::{
    execution_failure, lease_required, read_only_store, unavailable, ExclusiveExecutionGuard,
    ExecutionWriteGuard,
};

/// 一次写入准入持有的范围。
pub(super) enum WriteScope {
    /// 读侧门禁：允许同 root 的多个 mutation 并发。
    Concurrent(Option<ExecutionWriteGuard>),
    /// 写侧门禁：检查与写入之间不允许插入其他 mutation。
    Exclusive(Option<ExclusiveExecutionGuard>),
}

impl WriteScope {
    /// 按效果结清：只有证明「已生效」或「未生效」才 `finish`；`Unknown` 直接丢弃，
    /// 由 `Drop` 在租约上留下未决证据（之后的写入与 clean 都会被拒绝）。
    pub(super) fn settle<T>(self, result: &SessionResourceResult<T>) {
        let determinate = match result {
            Ok(_) => true,
            Err(error) => error.effect() != MutationOutcome::Unknown,
        };
        if !determinate {
            return;
        }
        match self {
            Self::Concurrent(Some(guard)) => guard.finish(),
            Self::Exclusive(Some(guard)) => guard.finish(),
            Self::Concurrent(None) | Self::Exclusive(None) => {}
        }
    }
}

/// 写入准入闸门。
///
/// 两个端口各持一种事实：`data` 是 canonical 会话数据（本机 SQLite 或远端 adapter）、
/// `local` 是本机执行面（发现、owner、代际、锁）。组合层决定两者指向哪个后端，
/// 闸门自己不做后端判断，也不持有任何 store 身份——v10 之后没有「本次服务哪个 store」
/// 这回事。
#[derive(Clone)]
pub(super) struct MutationGate {
    data: Arc<dyn SessionDataPort>,
    local: Arc<dyn LocalExecutionPort>,
    lifecycle: Lifecycle,
}

impl MutationGate {
    pub(super) fn new(
        data: Arc<dyn SessionDataPort>,
        local: Arc<dyn LocalExecutionPort>,
        lifecycle: Lifecycle,
    ) -> Self {
        Self {
            data,
            local,
            lifecycle,
        }
    }

    pub(super) fn data(&self) -> &Arc<dyn SessionDataPort> {
        &self.data
    }

    pub(super) fn local(&self) -> &Arc<dyn LocalExecutionPort> {
        &self.local
    }

    /// 新写入是否仍被接纳：只有 `Open` 放行。
    ///
    /// `Closing` 与 `Closed` 都拒绝——「停止新写入」从进入关闭流程起就不可逆；
    /// 恢复与排空不走这里（见 [`Self::ensure_recovery_permitted`]）。
    pub(super) fn ensure_open(&self) -> SessionResourceResult<()> {
        if self.lifecycle.state() != LifecycleState::Open {
            return Err(unavailable("session resources are closed"));
        }
        Ok(())
    }

    /// 恢复与排空的门禁：`Closing` 仍放行。
    ///
    /// 关闭过程本身要先收敛未结清事实（在途写入的屏障、租约上的未决标记），发起收敛的
    /// owner 也必须能在第一次关闭失败后继续推进；只有确认关闭（`Closed`）之后资源才
    /// 不再服务这些收敛行为。
    pub(super) fn ensure_recovery_permitted(&self) -> SessionResourceResult<()> {
        if self.lifecycle.state() == LifecycleState::Closed {
            return Err(unavailable("session resources are closed"));
        }
        Ok(())
    }

    /// 已有会话上的写入：只读打开让执行权不可得（历史仍可读）。
    pub(super) fn ensure_session_write(&self) -> SessionResourceResult<()> {
        self.ensure_open()?;
        if self.local.is_read_only() {
            return Err(read_only_store());
        }
        Ok(())
    }

    /// 需要登记新身份或新绑定的写入：只读打开连会话都还没有，没有可降级的对象。
    pub(super) fn ensure_registration_write(&self) -> SessionResourceResult<()> {
        self.ensure_open()?;
        if self.local.is_read_only() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::ReadOnlyStore),
            ));
        }
        Ok(())
    }

    /// 完整准入：能力/权限 → 本 root owner。
    ///
    /// 传给执行面的是**数据面事实**：调用方给的 `id` 可能是子会话，树根要在这里解析，
    /// 「有没有绑定」也只能由数据面回答（远端组合里本机没有这条会话的任何行）。
    pub(super) async fn admit(&self, id: &ThreadId) -> SessionResourceResult<WriteScope> {
        self.ensure_session_write()?;
        let facts = self.session_facts(id).await?;
        let guard = self
            .local
            .write_guard(id, &facts)
            .await
            .map_err(execution_failure)?;
        Ok(WriteScope::Concurrent(guard))
    }

    /// 统一写入：准入 → 执行 → 按效果结清。
    pub(super) async fn with_mutation<T, F, Fut>(
        &self,
        id: &ThreadId,
        work: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        let scope = self.admit(id).await?;
        let result = work().await;
        scope.settle(&result);
        result
    }

    /// 排他写入：与 [`Self::with_mutation`] 相同的准入判定，但「检查 + 写入」之间不允许
    /// 插入其他 mutation；因此要求存在活 owner（没有 owner 时不能承诺串行）。
    pub(super) async fn with_exclusive<T, F, Fut>(
        &self,
        id: &ThreadId,
        work: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        self.ensure_session_write()?;
        let facts = self.session_facts(id).await?;
        let guard = self
            .local
            .exclusive_guard(id, &facts)
            .await
            .map_err(execution_failure)?;
        let Some(guard) = guard else {
            return Err(lease_required());
        };
        let scope = WriteScope::Exclusive(Some(guard));
        let result = work().await;
        scope.settle(&result);
        result
    }

    /// 执行面判定要用的数据面事实（绑定字节、这棵树有没有绑定、树根）。
    ///
    /// 三件事都由数据端口回答：本机组合来自本机 `session_bindings` 与 `threads`，远端组合来自
    /// 远端会话行自带的绑定列与远端父链。执行面**不**查本机会话表——远端会话在本机没有行。
    ///
    /// 绑定取**这条会话自己的**（取得所有权时要在本机复核它的字节）；「这棵树有没有绑定」在
    /// 自身无绑定时再看 root 的——接纳过的 legacy root 可以有自己没有绑定行的子会话，那些子
    /// 会话的写入同样落在 root 的执行域里，不能因为「自己没有绑定」就当成无主放行。
    pub(super) async fn session_facts(&self, id: &ThreadId) -> SessionResourceResult<SessionFacts> {
        let binding = self.data.binding_of(id).await?;
        let root = self.data.session_root(id).await?;
        let bound = match &binding {
            Some(_) => true,
            None if root == *id => false,
            None => self.data.binding_of(&root).await?.is_some(),
        };
        Ok(SessionFacts {
            binding,
            bound,
            root,
        })
    }
}
