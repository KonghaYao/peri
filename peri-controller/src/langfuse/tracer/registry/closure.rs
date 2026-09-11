//! Stop/ToolEnded 双信号回收与 turn 结束兜底；异常诊断不抹去关闭义务。

use super::super::tool_batch::{ToolBatch, ToolsBatchFlush};
use super::{
    ActiveSubagent, ClosedSubagent, IncompleteReason, StartPending, SubagentRegistry,
    SubagentStatus, SubagentStopInfo,
};

impl SubagentRegistry {
    /// 父 ToolEnded:结束 invocation(不关闭 AGENT obs)。两信号齐备(Stop 已到)
    /// 时回收:flush child tool_batch → Closed → 返回关闭信息。
    pub(crate) fn on_invocation_tool_end(
        &mut self,
        agent_id: &str,
        tool_call_id: &str,
        output: &str,
        _is_error: bool,
    ) -> Option<ClosedSubagent> {
        let key = (agent_id.to_string(), tool_call_id.to_string());
        let inv = self.invocations.get_mut(&key)?;
        inv.tool_ended = true;
        inv.deferred_output = Some(output.to_string());
        if !inv.stop_received {
            return None; // 等 Stop 到达后再回收
        }
        let child = inv.bound_child.clone();
        let deferred = inv.deferred_output.clone();
        self.invocations.remove(&key);
        self.unbounded_invocations.retain(|k| k != &key);
        let child = child?;
        let sa = self.by_agent_id.get_mut(&child)?;
        if !matches!(
            sa.status,
            SubagentStatus::StopReceived | SubagentStatus::Active
        ) {
            return None;
        }
        let flush = std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush();
        let closed = finish_observation(sa, &child, deferred, flush, None);
        Some(closed)
    }

    /// 内容事件归属 AGENT obs 时刷新最后子内容时刻(处理时刻)。
    /// 仅更新,不强制;close 时 end_time = max(stop_time, last_content_time)。
    pub(crate) fn touch_content_time(&mut self, agent_id: &str, ts: &str) {
        let Some(sa) = self.by_agent_id.get_mut(agent_id) else {
            return;
        };
        sa.last_content_time = later_rfc3339(&sa.last_content_time, ts);
    }

    /// SubagentStop:按状态迁移(暂存/StopReceived/回收),返回关闭信息(如有)
    pub(crate) fn on_subagent_stop(
        &mut self,
        _parent_agent_id: &str,
        child_agent_id: &str,
        result: &str,
        is_error: bool,
    ) -> Option<ClosedSubagent> {
        let stop = SubagentStopInfo {
            result: result.to_string(),
            is_error,
            stop_time: chrono::Utc::now().to_rfc3339(),
        };
        let Some(sa) = self.by_agent_id.get_mut(child_agent_id) else {
            tracing::warn!(
                target: "langfuse::subagent",
                %child_agent_id,
                "SubagentStop 无对应 Start(丢失/乱序),标记 MissingStart"
            );
            self.incomplete_count += 1;
            return None;
        };
        match sa.status {
            SubagentStatus::PendingInvocation => {
                // Start 已入 pending、Stop 先到:暂存,等 join 后补 obs 生命周期
                sa.stop = Some(stop);
                None
            }
            SubagentStatus::Active => {
                let key = sa.invocation_key.clone();
                sa.stop = Some(stop);
                // 同步更新 invocation 的 stop_received(join 时的快照可能早于 Stop)
                if let Some(key) = &key {
                    if let Some(inv) = self.invocations.get_mut(key) {
                        inv.stop_received = true;
                    }
                }
                if let Some(key) = key {
                    if self
                        .invocations
                        .get(&key)
                        .map(|i| i.tool_ended)
                        .unwrap_or(false)
                    {
                        return self.close_subagent(child_agent_id);
                    }
                }
                sa.status = SubagentStatus::StopReceived;
                None
            }
            SubagentStatus::StopReceived => {
                tracing::warn!(
                    target: "langfuse::subagent",
                    %child_agent_id,
                    "SubagentStop 重复(已 StopReceived),标记 DuplicateStop"
                );
                self.mark_incomplete(child_agent_id, IncompleteReason::DuplicateStop);
                None
            }
            SubagentStatus::Closed | SubagentStatus::Incomplete(_) => {
                tracing::warn!(
                    target: "langfuse::subagent",
                    %child_agent_id,
                    "SubagentStop 到达但已关闭/终态,标记 DuplicateStop"
                );
                self.mark_incomplete(child_agent_id, IncompleteReason::DuplicateStop);
                None
            }
        }
    }

    /// 关闭 AGENT obs + 回收 invocation(两信号齐备或兜底时调用)
    pub(super) fn close_subagent(&mut self, child_agent_id: &str) -> Option<ClosedSubagent> {
        let sa = self.by_agent_id.get_mut(child_agent_id)?;
        if !matches!(
            sa.status,
            SubagentStatus::Active | SubagentStatus::StopReceived
        ) {
            return None;
        }
        let flush = std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush();
        let (key, deferred) = match &sa.invocation_key {
            Some(k) => {
                let deferred = self
                    .invocations
                    .get(k)
                    .and_then(|i| i.deferred_output.clone());
                (Some(k.clone()), deferred)
            }
            None => (None, None),
        };
        let closed = finish_observation(sa, child_agent_id, deferred, flush, None);
        if let Some(key) = key {
            self.invocations.remove(&key);
            self.unbounded_invocations.retain(|k| k != &key);
        }
        Some(closed)
    }

    // ── on_turn_end 兜底 ─────────────────────────────────────────────────────

    /// 清理未收 Stop 的活跃条目、pending Start、gate 缓存与残留 invocation。
    /// 返回兜底关闭的 AGENT obs(metadata 携带 incomplete_reason)。
    pub(crate) fn cleanup_turn_end(&mut self) -> Vec<ClosedSubagent> {
        let mut closed = Vec::new();
        // 1. pending Start 未 join → ParentLost(父 ToolStart 丢失)
        let pending: Vec<StartPending> = self.pending_starts.drain(..).collect();
        for sp in pending {
            self.mark_incomplete(&sp.child_agent_id, IncompleteReason::ParentLost);
        }
        // 2. gate 缓存残留(Start 从未到达)→ UnknownAgent(按 agent 去重计数)
        let remaining: std::collections::HashSet<String> =
            self.gate_cache.drain(..).map(|(agent, _)| agent).collect();
        for agent in remaining {
            self.mark_incomplete(&agent, IncompleteReason::UnknownAgent);
        }
        // 3. 关闭所有已打开且尚未关闭的观测，包括已有 Incomplete 诊断的条目
        let active: Vec<String> = self
            .by_agent_id
            .iter()
            .filter(|(_, sa)| !sa.observation_id.is_empty() && !sa.observation_closed)
            .map(|(k, _)| k.clone())
            .collect();
        for agent in active {
            let flush = self
                .by_agent_id
                .get_mut(&agent)
                .map(|sa| std::mem::replace(&mut sa.tool_batch, ToolBatch::new()).flush())
                .unwrap_or_else(|| ToolBatch::new().flush());
            let (key, deferred) = {
                let sa = self.by_agent_id.get(&agent).unwrap();
                let deferred = match &sa.invocation_key {
                    Some(k) => self
                        .invocations
                        .get(k)
                        .and_then(|i| i.deferred_output.clone()),
                    None => None,
                };
                (sa.invocation_key.clone(), deferred)
            };
            let sa = self.by_agent_id.get_mut(&agent).unwrap();
            let reason = match &sa.status {
                SubagentStatus::Incomplete(reason) => reason.clone(),
                _ => IncompleteReason::MissingStop,
            };
            closed.push(finish_observation(
                sa,
                &agent,
                deferred,
                flush,
                Some(reason),
            ));
            if let Some(key) = key {
                self.invocations.remove(&key);
                self.unbounded_invocations.retain(|k| k != &key);
            }
        }
        // 4. 残留 invocation(Start 丢失)→ 清除
        if !self.invocations.is_empty() {
            tracing::warn!(
                target: "langfuse::subagent",
                left = self.invocations.len(),
                "on_turn_end:残留未绑定 invocation 清除(Start 丢失)"
            );
            self.invocations.clear();
            self.unbounded_invocations.clear();
        }
        closed
    }
}

/// 所有正常/异常收尾共用的投影入口；调用方决定批次 flush 和 invocation 回收次序。
fn finish_observation(
    sa: &mut ActiveSubagent,
    child_agent_id: &str,
    deferred: Option<String>,
    flush: ToolsBatchFlush,
    incomplete_reason: Option<IncompleteReason>,
) -> ClosedSubagent {
    let stop = sa.stop.take().unwrap_or_else(|| SubagentStopInfo {
        result: String::new(),
        is_error: true,
        stop_time: chrono::Utc::now().to_rfc3339(),
    });
    let output = if stop.result.is_empty() {
        deferred.unwrap_or_default()
    } else {
        stop.result.clone()
    };
    let closed = ClosedSubagent {
        agent_id: child_agent_id.to_string(),
        observation_id: sa.observation_id.clone(),
        parent_observation_id: sa.parent_observation_id.clone(),
        start_time: sa.start_time.clone(),
        agent_name: sa.agent_name.clone(),
        input: sa.input.clone(),
        output,
        stop_time: later_rfc3339(&stop.stop_time, &sa.last_content_time),
        is_error: stop.is_error,
        flush,
        incomplete_reason,
    };
    sa.observation_closed = true;
    if !matches!(sa.status, SubagentStatus::Incomplete(_)) {
        sa.status = SubagentStatus::Closed;
    }
    closed
}

/// 两个 rfc3339 字符串取较晚者(解析失败回退前者)。
/// 时间均为 `Utc::now().to_rfc3339()` 生成,解析后比较避免字符串字典序
/// 在"整秒无小数 vs 带小数"时误判。
fn later_rfc3339(a: &str, b: &str) -> String {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(x), Ok(y)) => {
            if y > x {
                b.to_string()
            } else {
                a.to_string()
            }
        }
        (Ok(_), Err(_)) => a.to_string(),
        (Err(_), Ok(_)) => b.to_string(),
        (Err(_), Err(_)) => a.to_string(),
    }
}
