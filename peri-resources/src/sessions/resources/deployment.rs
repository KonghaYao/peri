use super::*;

// ─── 部署关闭 ─────────────────────────────────────────────────────────────────

impl SessionResourcesImpl {
    /// 关闭整个存储（部署生命周期行为，不属于业务行为面）。
    ///
    /// 完成条件有两条，缺一不算确认：**本机传输面的关闭走完**（数据面 `close` 成功返回），
    /// 以及**本实例登记的持久化未决已结清**——调用方丢弃 Arc、运行句柄已停止，
    /// 都不能抹掉登记中仍保留的未知写入效果。
    /// 未结清时保持 `Closing`（恢复入口仍可用），重复关闭重新做一遍真实检查。
    ///
    /// 权限不由本方法决定而由**谁能拿到实例**决定：业务侧只持有
    /// `Arc<dyn SessionResources>`（[`SessionResources`] 已不含关闭），本方法只对
    /// 具体实例可见，取用点只有部署 owner [`SessionStoreShutdownOwner`]。
    ///
    /// [`SessionStoreShutdownOwner`]: crate::context::SessionStoreShutdownOwner
    pub(crate) async fn close(&self) -> SessionResourceResult<()> {
        // 关闭确认串行化：后来者要么看到确认关闭（幂等成功），要么重新做一遍真实检查，
        // 不会因为「上一次调用过」或「正好并发」而绕过未结清事实。
        let _confirm = self.close_confirm.lock().await;
        if self.lifecycle.state() == LifecycleState::Closed {
            return Ok(());
        }
        // 停止新写入不可逆；`Closing` 不是 `Closed`：未结清事实仍可收敛（恢复/排空在
        // `Closing` 下继续可用，见 `gate::ensure_recovery_permitted`）。
        self.lifecycle.begin_closing();
        let _credentials = peri_time::timeout(SETTLE_WAIT, self.credential_operations.write())
            .await
            .map_err(|_| SessionResourceError::new(SessionResourceErrorKind::Timeout))?;

        // 每次调用都重新做真实检查，不复用上一次的失败结论。
        //
        for lease in self.gate.local().live_leases() {
            if peri_time::timeout(SETTLE_WAIT, lease.wait_for_in_flight())
                .await
                .is_err()
            {
                return Err(SessionResourceError::new(SessionResourceErrorKind::Timeout));
            }
            if lease.is_uncertain() {
                return Err(SessionResourceError::persistence_uncertain(Some(
                    lease.thread_id().clone(),
                )));
            }
        }
        // 恢复所需的证据已确认结清之后才关闭数据面：提前取走连接（远程 adapter 的唯一
        // 连接句柄）会让「未确认」的未决事实失去收敛路径，而重复关闭恰恰要能重做检查。
        self.gate.data().close().await?;
        // 只有到这里才是确认关闭（并发调用由串行化保证只有一个走到这里；即便迁移已由
        // 别处完成，结论也相同）。本机数据面的 `close` 只停止它自己的写入入口（连接池由
        // 共享库句柄所有）；clean 由各 owner 自己写（`SessionExecutionLease::mark_clean`），
        // 门面不代写、也不替它们宣告会话已结清。
        self.lifecycle.confirm_closed();
        Ok(())
    }
}
