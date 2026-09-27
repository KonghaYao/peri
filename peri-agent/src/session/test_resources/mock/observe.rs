//! 故障注入与观察：只针对被测试的行为，未覆盖的行为由门面替身按契约拒绝。

use super::*;

impl MockSessionResources {
    /// 在第 `nth` 次快照读取（1-based）注入失败；0 关闭注入。
    pub(crate) fn fail_snapshot_load_at(&self, nth: usize) {
        self.fail_snapshot_at.store(nth, Ordering::SeqCst);
    }

    pub(crate) fn fail_compaction(self: &Arc<Self>) {
        self.injection.lock().unwrap().fail_compaction = true;
    }

    /// 全部会话 payload 的合并视图（登记顺序），夹具断言用。
    pub(crate) fn payloads(&self) -> Vec<PersistedPayload> {
        let regions = self.regions.lock().unwrap();
        self.order
            .lock()
            .unwrap()
            .iter()
            .filter_map(|id| regions.get(id))
            .flat_map(|region| region.payloads.clone())
            .collect()
    }

    /// 指定会话的 payload（未登记 id 按空区回答，仅夹具使用）。
    pub(crate) fn payloads_of(&self, id: &ThreadId) -> Vec<PersistedPayload> {
        self.region(id).map(|r| r.payloads).unwrap_or_default()
    }

    pub(crate) fn flags(&self, id: &MessageId) -> MessageFlags {
        self.regions
            .lock()
            .unwrap()
            .values()
            .find_map(|region| region.flags.get(id).cloned())
            .unwrap_or_default()
    }

    /// 指定会话的 flags（未登记 id 按空区回答，仅夹具使用）。
    pub(crate) fn flags_of(&self, id: &ThreadId) -> HashMap<MessageId, MessageFlags> {
        self.region(id).map(|r| r.flags).unwrap_or_default()
    }

    pub(crate) fn append_calls(&self) -> usize {
        self.injection.lock().unwrap().append_calls
    }

    pub(crate) fn compaction_calls(&self) -> usize {
        self.injection.lock().unwrap().compaction_calls
    }

    pub(crate) fn rewind_calls(&self) -> usize {
        self.injection.lock().unwrap().rewind_calls
    }
}
