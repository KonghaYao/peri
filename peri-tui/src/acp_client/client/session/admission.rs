//! Queue-side session admission: identity resolution under the operation gate.

use peri_acp::transport::types::AcpError;

use crate::acp_client::client::{AcpTuiClient, AdmissionRoute};
use crate::acp_client::interaction_lifecycle::{SessionOperationId, TransitionOperation};

/// 入队时的会话准入快照。
pub(crate) struct SessionAdmissionSnapshot {
    pub(crate) route: AdmissionRoute,
    /// 有尚未释放的加载预约：prompt 必须等它们消费完。
    pub(crate) pending_load: bool,
    /// 最新一个尚未被 load 认领的预约：它代表用户最新的会话决定。
    pub(crate) unclaimed_load: Option<SessionOperationId>,
    /// 用户会话决策序号；变化即表示这份输入已被新的决定取代。
    pub(crate) switch_seq: u64,
}

/// 队列 cohort 的会话准入约束。
#[derive(Clone, Copy, Debug)]
pub(crate) struct AdmissionGuard {
    switch_seq: u64,
    /// 无会话时是否允许建立；等待既有切换的 cohort 必须为 false。
    allow_create: bool,
    /// cohort 等待的操作身份；落定身份必须由同一操作产生。
    operation: Option<SessionOperationId>,
}

impl AdmissionGuard {
    pub(crate) fn new(switch_seq: u64, allow_create: bool) -> Self {
        Self {
            switch_seq,
            allow_create,
            operation: None,
        }
    }

    pub(crate) fn with_operation(mut self, operation: SessionOperationId) -> Self {
        self.operation = Some(operation);
        self
    }
}

/// cohort 已登记身份时沿用；没有 cohort 的直接调用接续当前待定身份。
fn operation_of(operation: Option<SessionOperationId>) -> TransitionOperation {
    operation.map_or(
        TransitionOperation::AdoptPending,
        TransitionOperation::Owned,
    )
}

impl AcpTuiClient {
    /// Return the current stable session, creating it only if no startup restore
    /// owns the lifecycle decision.  Both the stable re-check and a possible
    /// `session/new` execute under the client operation gate.
    pub async fn ensure_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        self.ensure_session_identity(cwd, model)
            .await
            .map(|(session_id, _)| session_id)
    }

    /// 建立/发现会话并返回在 operation gate 内确定的完整身份。
    ///
    /// 没有 cohort 归属的直接提交用它取身份：`ensure_session` 在返回后再读一次
    /// `stable_identity` 会跨出本次决定的临界区，同 ID 重载可能读到新 generation。
    pub(crate) async fn ensure_session_identity(
        &self,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<(String, u64), AcpError> {
        self.ensure_session_inner(cwd, model, None, |_| {})
            .await?
            .ok_or_else(|| AcpError::new(-32603, "session admission was invalidated"))
    }

    /// 提交队列的会话准入快照。
    ///
    /// 预约、决策序号与路由在同一把锁内读出（`reservations → lifecycle`）：
    /// `reserve_session_load` 与认领/切换临界区也按同一顺序写入，因此快照不会
    /// 观察到「已认领但路由仍是旧 Stable」的撕裂组合。`unclaimed_load` 暴露的是
    /// 最新未认领预约的操作身份，路由切换不再由「存在未认领预约」预测代际。
    pub(crate) fn session_admission_snapshot(&self) -> SessionAdmissionSnapshot {
        let reservations = self.session_load_reservations.reservations.lock().unwrap();
        let pending_load = reservations.pending() > 0;
        let unclaimed_load = reservations.newest_unclaimed();
        let switch_seq = self.current_session_switch_seq();
        let route = self.lifecycle.admission_route();
        SessionAdmissionSnapshot {
            route,
            pending_load,
            unclaimed_load,
            switch_seq,
        }
    }

    /// 在生命周期同步内落定 cohort 的会话身份。
    ///
    /// `commit` 在 operation gate 内、与身份选择同一临界区执行：拿到 `Some` 的
    /// cohort 得到的是本次决定的完整身份；拿到 `None` 表示该 cohort 已被新的用户
    /// 会话决定取代（或它等待的切换没有落定），必须永久取消，不得把空槽当作
    /// 「尚未初始化」再次尝试。
    pub(crate) async fn resolve_session_admission(
        &self,
        cwd: &str,
        admission: AdmissionGuard,
        commit: impl FnOnce(Option<&(String, u64)>),
    ) -> Result<(), AcpError> {
        self.ensure_session_inner(cwd, None, Some(admission), commit)
            .await
            .map(|_| ())
    }

    /// `ensure_session` / cohort 准入的共享实现。
    ///
    /// 有 `admission` 时，任何一次身份选择都必须先确认用户决策序号未变；变化即
    /// 返回 `None` 由调用方永久取消该 cohort。等待既有切换的 cohort
    /// （`allow_create == false`）只复用落定结果，绝不代用户新建会话。
    async fn ensure_session_inner(
        &self,
        cwd: &str,
        model: Option<&str>,
        admission: Option<AdmissionGuard>,
        commit: impl FnOnce(Option<&(String, u64)>),
    ) -> Result<Option<(String, u64)>, AcpError> {
        let mut startup_restore_rx = self.startup_restore_tx.subscribe();
        let mut load_epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let operation = self.lifecycle.operation_gate().lock().await;
            let (pending_load, stable_session) = {
                let _ = *load_epoch_rx.borrow_and_update();
                let reservations = self.session_load_reservations.reservations.lock().unwrap();
                // 决策序号与预约计数同锁读取：reserve_session_load 在同一临界区
                // 递增两者，因此这里不会读到「新预约 + 旧决策」的撕裂组合。
                if admission.as_ref().is_some_and(|admission| {
                    admission.switch_seq != self.current_session_switch_seq()
                }) {
                    commit(None);
                    return Ok(None);
                }
                if reservations.pending() > 0 {
                    (true, None)
                } else {
                    // Hold the reservation mutex through Stable selection so a
                    // dispatcher linearizes strictly before or after this result.
                    (false, self.lifecycle.stable_identity_with_operation())
                }
            };
            if pending_load {
                drop(operation);
                self.wait_for_session_load(&mut load_epoch_rx).await?;
                continue;
            }
            if let Some((session_id, generation, operation)) = stable_session {
                if let Err(error) = self.check_restore_error() {
                    // 粘滞的恢复失败没有可用身份：准入 cohort 只能作废，
                    // 不能退回「无会话」再建立一次。
                    if admission.is_some() {
                        commit(None);
                        return Ok(None);
                    }
                    return Err(error);
                }
                // cohort 只采纳自己等待的操作产生的身份：同代际的其它切换或
                // 重载不得把已排队消息改投过去。
                if let Some(expected) = admission.and_then(|admission| admission.operation)
                    && operation != Some(expected)
                {
                    commit(None);
                    return Ok(None);
                }
                let identity = (session_id, generation);
                commit(Some(&identity));
                return Ok(Some(identity));
            }
            if *startup_restore_rx.borrow_and_update() {
                drop(operation);
                startup_restore_rx.changed().await.map_err(|_| {
                    AcpError::new(-32603, "startup restore reservation closed unexpectedly")
                })?;
                continue;
            }
            // 以下分支会自行改变会话集合。等待已预约/进行中切换的 cohort 不得
            // 代劳：它只认那次切换的结果，失败就取消。
            if admission
                .as_ref()
                .is_some_and(|admission| !admission.allow_create)
            {
                commit(None);
                return Ok(None);
            }
            // 以下分支会自行改变会话集合；cohort 的操作身份贯穿这次切换。
            let operation = admission.and_then(|admission| admission.operation);
            // 启动恢复失败但目标仍保留：先重试同一会话一次，不静默新建。
            if let Some(target) = self.startup_retry_target() {
                let identity = self
                    .load_identity_under_gate(&target, cwd, model, operation)
                    .await?;
                commit(identity.as_ref());
                return Ok(identity);
            }
            self.check_restore_error()?;
            // 上一次加载 B 被关闭失败打断：先完成同一加载（`load_session_under_gate`
            // 内部会先重放关闭 A），不要把用户的历史意图换成新建空会话。
            if let Some(target) = self.pending_resume_load() {
                let identity = self
                    .load_identity_under_gate(&target, cwd, model, operation)
                    .await?;
                commit(identity.as_ref());
                return Ok(identity);
            }
            let identity = self
                .create_identity_under_gate(cwd, model, operation)
                .await?;
            commit(identity.as_ref());
            return Ok(identity);
        }
    }

    /// 加载会话并返回提交落定的完整身份（generation 在同一次 `commit_stable`
    /// 内确定）。身份与请求的会话不一致时返回 `None`，绝不把别的会话交出去。
    async fn load_identity_under_gate(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
        operation: Option<SessionOperationId>,
    ) -> Result<Option<(String, u64)>, AcpError> {
        let loaded = self
            .load_session_under_gate(session_id, cwd, model, None, operation_of(operation))
            .await?;
        Ok(self
            .lifecycle
            .stable_identity()
            .filter(|(stable_id, _)| stable_id == &loaded))
    }

    async fn create_identity_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
        operation: Option<SessionOperationId>,
    ) -> Result<Option<(String, u64)>, AcpError> {
        let created = self
            .new_session_under_gate(cwd, model, operation_of(operation))
            .await?;
        Ok(self
            .lifecycle
            .stable_identity()
            .filter(|(stable_id, _)| stable_id == &created))
    }

    fn current_session_switch_seq(&self) -> u64 {
        self.session_switch_seq
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// 当前用户会话决策序号；队列提交用它快照入队时的意图。
    pub(crate) fn session_switch_seq(&self) -> u64 {
        self.current_session_switch_seq()
    }

    /// 同一操作 cohort 已永久取消时为新的输入分配新身份。
    pub(crate) fn renew_session_operation(&self) -> SessionOperationId {
        self.lifecycle.allocate_operation()
    }

    /// 无车道的未绑定提交（新式服务端的远程命令）：在决策守卫下建立/等待会话。
    ///
    /// 与 cohort 解析共用同一入口：决策序号一旦变化即返回 `None`，绝不把命令
    /// 改投到此后才建立的会话。
    pub(crate) async fn resolve_unbound_admission(
        &self,
        cwd: &str,
        switch_seq: u64,
    ) -> Result<Option<(String, u64)>, AcpError> {
        self.ensure_session_inner(
            cwd,
            None,
            Some(AdmissionGuard::new(switch_seq, true)),
            |_| {},
        )
        .await
    }

    pub(super) fn bump_session_switch_seq(&self) {
        self.session_switch_seq
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}
