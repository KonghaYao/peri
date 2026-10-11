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

        self.gate.drain_all().await?;
        // 恢复所需的证据已确认结清之后才关闭数据面：提前取走连接（远程 adapter 的唯一
        // 连接句柄）会让「未确认」的未决事实失去收敛路径，而重复关闭恰恰要能重做检查。
        self.gate.data().close().await?;
        self.lifecycle.confirm_closed();
        Ok(())
    }
}
