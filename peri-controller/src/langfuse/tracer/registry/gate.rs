//! 有界注册闸门及按原顺序取出的重放事件。
//!
//! 闸门保存的 LLM 事件携带已冻结的输入来源：计费在入闸门前完成，闸门本身不做 IO，
//! 也不持有 messages/tools 或请求体原文。逐出与拒绝都立即释放对应来源（permit 归还 /
//! ticket 置取消位），因此缓存满不会让预算被静默占住（契约 §3.3）。

use super::{GateEvent, IncompleteReason, SubagentRegistry, GATE_CACHE_LIMIT};

impl SubagentRegistry {
    // ── 注册闸门(有界缓存 + 重放) ─────────────────────────────────────────────

    /// 缓存一条未知 agent 的内容事件。返回 true = 已缓存(等待 Start join 重放);
    /// false = 缓存失败(溢出逐出后拒绝)。
    pub(crate) fn try_gate(&mut self, ev: GateEvent) -> bool {
        let agent_id = ev.agent_id().to_string();
        if self.gate_cache.len() >= GATE_CACHE_LIMIT {
            // 溢出:逐出最旧事件(丢弃,不重放);若其 agent 的 Start 正在等待 join
            // (pending_starts),该 child 已无法完整重放 → 标 CacheOverflow。
            if let Some((evicted_agent, evicted)) = self.gate_cache.pop_front() {
                // 逐出即释放该事件持有的冻结来源（不等待 IO，不等 backlog 排空）。
                drop(evicted);
                if self
                    .pending_starts
                    .iter()
                    .any(|sp| sp.child_agent_id == evicted_agent)
                {
                    self.pending_starts
                        .retain(|sp| sp.child_agent_id != evicted_agent);
                    self.mark_incomplete(&evicted_agent, IncompleteReason::CacheOverflow);
                }
                tracing::warn!(
                    target: "langfuse::subagent",
                    %evicted_agent,
                    gate_len = self.gate_cache.len(),
                    "gate_cache 溢出,逐出最旧事件(不重放)"
                );
            }
            // 缓存满时拒绝新事件:直接丢弃,按未知处理(其冻结来源在此释放)
            tracing::warn!(
                target: "langfuse::subagent",
                %agent_id,
                "gate_cache 已满,拒绝缓存新事件(丢弃,不挂主 agent)"
            );
            return false;
        }
        self.gate_cache.push_back((agent_id, ev));
        true
    }

    /// 取出该 child 的全部 gate 缓存事件(按原顺序),并从缓存移除。
    /// 取出的来源由重放方负责交付或释放。
    pub(crate) fn take_gated_events(&mut self, child_agent_id: &str) -> Vec<GateEvent> {
        let mut taken = Vec::new();
        let mut i = 0;
        while i < self.gate_cache.len() {
            if self.gate_cache[i].0 == child_agent_id {
                let (_, ev) = self.gate_cache.remove(i).unwrap();
                taken.push(ev);
            } else {
                i += 1;
            }
        }
        taken
    }

    pub(crate) fn gated_len(&self) -> usize {
        self.gate_cache.len()
    }
}
