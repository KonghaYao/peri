use super::*;

impl RemoteSessionData {
    /// 装载故障计划（仅测试构建）：把「响应丢失」「发出前丢弃」变成可控观察点，
    /// 走的是同一套真实批、真实账本与真实恢复路径。
    #[cfg(test)]
    pub(in crate::sessions::remote) async fn inject_faults(
        &self,
        plan: crate::sessions::remote::mutation::FaultPlan,
    ) {
        if let Some(store) = self.slot.read().await.serving_store() {
            store.inject_faults(plan);
        }
    }

    /// 测试装配：连接已关闭的 adapter（不连网，与 `close` 之后的状态同一个形状）。
    ///
    /// 这种装配下任何一次 store 访问都只会失败，所以「输入不自洽时仍然拿到 `InvalidInput`」
    /// 就证明判定发生在取连接之前、也没有写下任何本机记录。
    ///
    /// 注意这与「关闭过一次但没成功」**不同**：那种情况下连接仍被保留在关闭句柄里，
    /// 关闭可以重试（见 [`RemoteSessionData::close`]）；这里从来没有过连接可关。
    #[cfg(test)]
    pub(in crate::sessions::remote) fn closed_for_test(store_id: StoreId) -> Self {
        Self {
            machine_id: crate::sessions::machine::current()
                .unwrap_or_default()
                .to_owned(),
            slot: RwLock::new(ConnectionSlot::default()),
            factory: Arc::new(NoConnectionFactory),
            gate: Arc::new(ConnectionGate::default()),
            store_id,
            schema_version: schema::REMOTE_SCHEMA_VERSION,
            roots: RwLock::new(HashMap::new()),
        }
    }

    /// 测试装配：连接由调用方给定的工厂与首条连接构成（故障可控，不连网）。
    ///
    /// 首条连接与工厂共用同一份代际门禁：失效、重建与「迟到任务不碰新连接」的判定与生产
    /// 完全一致，测试只是把传输面换成能确定复现故障的实现。
    #[cfg(test)]
    pub(in crate::sessions::remote) fn with_connection_for_test(
        store_id: StoreId,
        connection: RemoteStore,
        factory: Arc<dyn ConnectionFactory>,
        gate: Arc<ConnectionGate>,
    ) -> Self {
        Self {
            machine_id: crate::sessions::machine::current()
                .unwrap_or_default()
                .to_owned(),
            slot: RwLock::new(ConnectionSlot::serving(connection)),
            factory,
            gate,
            store_id,
            schema_version: schema::REMOTE_SCHEMA_VERSION,
            roots: RwLock::new(HashMap::new()),
        }
    }
}
