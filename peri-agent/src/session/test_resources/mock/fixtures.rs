//! 夹具便利方法：镜像迁移前 `ThreadStore` 的常用测试调用形态，让既有测试的构造与断言逐字保留；
//! 生产代码不再有这些方法，只有测试替身提供。

use super::*;

/// 便利方法：镜像迁移前 `ThreadStore` 的常用测试调用形态，让既有测试的构造与断言
/// 逐字保留（生产代码不再有这些方法，只有测试替身提供）。
///
/// **读侧分工**：trait 方法（`load_session_meta` / `load_session_snapshot`）按真实门面
/// 语义对未登记 id 回答 `NotFound`；这里的方法只服务夹具——读取未登记 id 时按「空区」
/// 回答，写入时按「写入即登记」补齐记录。需要「不存在」语义的用例请用 trait 方法。
impl MockSessionResources {
    pub(crate) async fn create_resumable_thread(
        &self,
        meta: ThreadMeta,
    ) -> Result<ThreadId, anyhow::Error> {
        let id = self.create_legacy_thread(meta).await?;
        Ok(id)
    }

    pub(crate) async fn create_legacy_thread(
        &self,
        meta: ThreadMeta,
    ) -> Result<ThreadId, anyhow::Error> {
        let id = self.create_thread(meta).await?;
        Ok(id)
    }

    pub(crate) async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId, anyhow::Error> {
        let id = meta.id.clone();
        if let Some(parent) = meta.parent_thread_id.clone() {
            // 生产不变量：父行先于子行存在（子会话的 root 解析沿 parent 链读取 meta）。
            // 替身按同一顺序补齐父链占位行，避免夹具出现断裂父链。
            self.ensure_parent_placeholder(&parent, &meta.cwd);
        }
        self.with_region(&id, |region| region.meta = Some(meta));
        Ok(id)
    }

    /// 补齐父链占位行（未登记时才写，已有记录不覆盖）。
    fn ensure_parent_placeholder(&self, parent: &str, cwd: &str) {
        let parent = parent.to_owned();
        if self.regions.lock().unwrap().contains_key(&parent) {
            return;
        }
        let mut meta = ThreadMeta::new_at(cwd, peri_time::now_wall());
        meta.id = parent.clone();
        self.with_region(&parent, |region| region.meta = Some(meta));
    }

    pub(crate) async fn append_messages(
        &self,
        id: &ThreadId,
        messages: &[BaseMessage],
    ) -> Result<(), anyhow::Error> {
        let payloads: Vec<PersistedPayload> = messages
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect();
        self.append_history(id, &payloads)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub(crate) async fn append_message(
        &self,
        id: &ThreadId,
        message: BaseMessage,
    ) -> Result<(), anyhow::Error> {
        self.append_history(id, &[PersistedPayload::Message(message)])
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub(crate) async fn load_messages(
        &self,
        id: &ThreadId,
    ) -> Result<Vec<BaseMessage>, anyhow::Error> {
        Ok(self
            .payloads_of(id)
            .iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect())
    }

    pub(crate) async fn load_message_flags(
        &self,
        id: &ThreadId,
    ) -> Result<HashMap<MessageId, MessageFlags>, anyhow::Error> {
        Ok(self.flags_of(id))
    }

    pub(crate) async fn load_payloads(
        &self,
        id: &ThreadId,
    ) -> Result<Vec<PersistedPayload>, anyhow::Error> {
        Ok(self.payloads_of(id))
    }

    /// 夹具读取 meta：与 trait 的 `load_session_meta` 同语义（未登记 id 报错）。
    pub(crate) async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta, anyhow::Error> {
        match self.region(id).and_then(|region| region.meta) {
            Some(meta) => Ok(meta),
            None => anyhow::bail!("thread {id} not found"),
        }
    }

    pub(crate) async fn update_thread_status(
        &self,
        id: &ThreadId,
        status: &str,
    ) -> Result<(), anyhow::Error> {
        // 未知状态值直接报错，不静默 fallback 成 active（与真实 store 的强类型语义一致：
        // 状态只有 Done/Cancelled/Error/Active 四种，拼错必须暴露）。
        let status = match status {
            "done" => peri_acp_types::thread::AgentStatus::Done,
            "cancelled" => peri_acp_types::thread::AgentStatus::Cancelled,
            "error" => peri_acp_types::thread::AgentStatus::Error,
            "active" => peri_acp_types::thread::AgentStatus::Active,
            other => anyhow::bail!("非法 agent_status: {other}"),
        };
        self.update_session_meta(
            id,
            &SessionMetaPatch {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
    }
}
