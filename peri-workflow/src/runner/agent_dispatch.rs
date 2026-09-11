//! agent/run 的注册、配额、取消与响应所有权。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use super::limits::try_reserve_live_attempt;
use super::run_protocol::parse_agent_run_params;
use super::AgentExecutor;
use crate::progress::WorkflowProgressStore;
use crate::protocol::{AgentRunResult, WorkflowLimits, ERR_ABORTED, ERR_INVALID_PARAMS};
use crate::rpc::RpcChannel;

pub(super) struct AgentDispatcher {
    pub(super) run_id: String,
    pub(super) agent_executor: Arc<dyn AgentExecutor>,
    pub(super) channel: Arc<RpcChannel>,
    pub(super) progress_store: Arc<WorkflowProgressStore>,
    pub(super) run_limits: WorkflowLimits,
    pub(super) run_started: std::time::Instant,
    pub(super) live_agent_attempts: Arc<AtomicU64>,
    pub(super) observed_tool_calls: Arc<AtomicU64>,
    pub(super) limit_breach: Arc<parking_lot::Mutex<Option<String>>>,
}

impl AgentDispatcher {
    // Invalid/duplicate requests are answered locally. Only a run limit ends dispatch.
    pub(super) async fn dispatch(
        &self,
        id: Option<u64>,
        params: Option<Value>,
    ) -> Result<(), String> {
        // Parse params, spawn agent execution
        let params = match parse_agent_run_params(params, &self.run_id) {
            Ok(params) => params,
            Err(error) => {
                warn!(
                    target: "workflow.rpc",
                    error = %error,
                    "agent/run rejected invalid params",
                );
                if let Some(id) = id {
                    let _ = self
                        .channel
                        .send_error(id, ERR_INVALID_PARAMS, "invalid agent/run params")
                        .await;
                }
                return Ok(());
            }
        };
        // Extract run_id + agent_id for kill tracking before moving params
        let agent_run_id = params.run_id.clone();
        let agent_id_num = params.agent_id;
        if self
            .run_limits
            .max_elapsed_ms
            .is_some_and(|maximum| self.run_started.elapsed().as_millis() as u64 >= maximum)
        {
            let error = format!(
                "workflow exceeded maxElapsedMs ({})",
                self.run_limits.max_elapsed_ms.unwrap_or_default()
            );
            *self.limit_breach.lock() = Some(error.clone());
            if let Some(id) = id {
                let _ = self
                    .channel
                    .send_error(id, ERR_ABORTED, "workflow maxElapsedMs limit exceeded")
                    .await;
            }
            return Err(error);
        }
        // 注册提前到 spawn 之前（GAP-07 原子化）：kill_agent 与
        // 注册之间不再有空窗（此前注册在 spawn 内，kill 先到会
        // 漏杀且返回 false）；duplicate 拒绝在 spawn 前完成，
        // 不产生孤儿 task。返回 (cancel_rx, 注册 token)。
        let Some((cancel_rx, reg_token)) =
            self.channel.register_agent(&agent_run_id, agent_id_num, id)
        else {
            warn!(
                target: "workflow.rpc",
                run_id = %agent_run_id,
                agent_id = agent_id_num,
                "agent/run rejected duplicate active agentId",
            );
            if let Some(id) = id {
                let _ = self
                    .channel
                    .send_error(id, ERR_INVALID_PARAMS, "duplicate active agentId")
                    .await;
            }
            return Ok(());
        };
        let Some(live_attempt_permit) =
            try_reserve_live_attempt(&self.live_agent_attempts, self.run_limits.max_agents)
        else {
            let _ = self
                .channel
                .deregister_agent(&agent_run_id, agent_id_num, reg_token);
            let error = format!(
                "workflow exceeded maxAgents ({})",
                self.run_limits.max_agents.unwrap_or_default()
            );
            *self.limit_breach.lock() = Some(error.clone());
            if let Some(id) = id {
                let _ = self
                    .channel
                    .send_error(id, ERR_ABORTED, "workflow maxAgents limit exceeded")
                    .await;
            }
            return Err(error);
        };
        let exec = Arc::clone(&self.agent_executor);
        let ch = Arc::clone(&self.channel);
        let progress_for_agent = Arc::clone(&self.progress_store);
        let tool_calls_for_agent = Arc::clone(&self.observed_tool_calls);
        let breach_for_agent = Arc::clone(&self.limit_breach);
        let max_tool_calls = self.run_limits.max_tool_calls;
        tokio::spawn(async move {
            // permit 随 task 生命周期持有；成功、失败、取消或 panic unwind
            // 都由 Drop 释放 live maxAgents 配额。
            let _live_attempt_permit = live_attempt_permit;
            // Execute with cancel support
            let result = tokio::select! {
                r = exec.execute(params) => r,
                _ = cancel_rx => {
                    crate::protocol::AgentRunResult::Dead {
                        reason: Some("killed".into()),
                        detail: Some("agent killed by user".into()),
                    }
                }
            };

            // 完成归属：仅当注册仍由本 task 持有（未被 kill_agent
            // 取走）时移除并返回 true；false 表示 kill 分支已发送
            // error response，本 task 不得再发成功响应。
            let owned = ch.deregister_agent(&agent_run_id, agent_id_num, reg_token);

            // 从 progress store 补注 phase：engine 的 phase() 上下文仅通过
            // progress 事件传递，不进入 AgentRunParams.phase（hooks.js:21 漏了）
            // → agent_started 事件包含 phase，进度 store 先收到，此处补入结果。
            let mut result = result;
            let reported_tool_count = result.tool_count();
            if let Some(tool_count) = reported_tool_count {
                let total = tool_calls_for_agent
                    .fetch_add(tool_count, Ordering::SeqCst)
                    .saturating_add(tool_count);
                if max_tool_calls.is_some_and(|maximum| total > maximum) {
                    let detail = format!(
                        "workflow exceeded maxToolCalls ({})",
                        max_tool_calls.unwrap_or_default()
                    );
                    *breach_for_agent.lock() = Some(detail.clone());
                    result = AgentRunResult::Dead {
                        reason: Some("resource-limit".into()),
                        detail: Some(detail),
                    };
                }
            }
            if let AgentRunResult::Ok { phase, .. } = &mut result {
                if phase.is_none() {
                    *phase = progress_for_agent.get_agent_phase(&agent_run_id, agent_id_num);
                }
            }

            // 响应门控：owned=false（注册已被 kill_agent 取走）或
            // killed 结果（kill 分支已发送 error response）都跳过
            // 响应，避免双重 JSON-RPC 响应违反协议规范。
            // 其他 Dead 变体（no-structured-output / interrupted /
            // runagent-threw）来自 executor 自身错误，仍需正常发送
            // 响应，否则 Node Promise 永远 hang
            if let Some(id) = id {
                let was_killed = matches!(
                    result,
                    AgentRunResult::Dead { reason: Some(ref r), .. } if r == "killed"
                );
                let resource_limited = matches!(
                    result,
                    AgentRunResult::Dead { reason: Some(ref r), .. } if r == "resource-limit"
                );
                if owned && resource_limited {
                    let _ = ch
                        .send_error(id, ERR_ABORTED, "workflow resource limit exceeded")
                        .await;
                } else if owned && !was_killed {
                    let result_val = serde_json::to_value(&result).unwrap_or_else(|_| {
                        serde_json::json!({
                            "kind": "dead",
                            "reason": "runagent-threw",
                            "detail": "serialize failed"
                        })
                    });
                    let _ = ch.send_response(id, result_val).await;
                }
            }
        });
        Ok(())
    }
}
