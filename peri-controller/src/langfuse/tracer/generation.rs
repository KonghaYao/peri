//! LLM Generation 生命周期追踪器。
//!
//! 管理单次 LLM 调用从 start → end 的完整生命周期状态：
//! - on_llm_start：创建 GenerationCached（持有已冻结的输入来源）
//! - on_llm_request_payload：用请求体来源替换 fallback，并释放原引用
//! - on_llm_retrying：累积重试记录
//! - on_llm_end：取出缓存数据（含输入来源），返回 GenerationEnd 供外层构造 TelemetryEvent
//!
//! 追踪器只保存 [`GenerationInput`]：小输入冻结为内存字节、大输入只持暂存句柄；结束/取消
//! 直接移动来源，不等 IO，也不克隆原文（契约 §3.3）。

use std::collections::HashMap;

use peri_model::TokenUsage;

use super::input_snapshot::GenerationInput;

/// 单个 generation 的缓存条目；`input` 不可 Clone/Debug（来源与 permit 私有）。
pub(crate) struct GenerationCached {
    pub gen_id: String,
    pub start_time: String,
    /// 首个推理分片时间；只有 provider 实际产生 reasoning 事件时才存在。
    pub completion_start_time: Option<String>,
    /// 初始 fallback（messages/tools）或请求体到达后替换成的实际请求来源。
    pub input: GenerationInput,
}

#[derive(Debug, Clone)]
pub(crate) struct RetryAttempt {
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay_ms: u64,
}

pub(crate) struct GenerationStart {
    pub gen_id: String,
    pub start_time: String,
}

pub(crate) struct GenerationEnd {
    pub gen_id: String,
    pub start_time: String,
    /// 首个推理分片时间；None 表示本次调用未观测到 reasoning。
    pub completion_start_time: Option<String>,
    pub input: GenerationInput,
    pub retry_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingTerminalState {
    pub model: String,
    pub output: String,
    pub usage: Option<TokenUsage>,
    pub request_id: Option<String>,
}

pub(crate) struct AbandonedGeneration {
    pub agent_id: String,
    pub step: usize,
    pub gen_id: String,
    pub start_time: String,
    /// 首个推理分片时间；兜底关闭时仍须保留，不能因走 fallback 丢失。
    pub completion_start_time: Option<String>,
    pub input: GenerationInput,
    pub retry_metadata: Option<serde_json::Value>,
    pub terminal: Option<PendingTerminalState>,
}

pub(crate) struct GenerationTracker {
    /// key = (agent_id, step)。并行 subagent 各自拥有独立 step 计数器，
    /// 若不区分 agent，step-N 缓存会互相覆盖导致 generation 错配。
    generation_data: HashMap<(String, usize), GenerationCached>,
    active_step: Option<usize>,
    /// key = (agent_id, step)。retry 记录按 generation 隔离：
    /// 并行 agent 交错时，on_llm_end 只消费自己 generation 的重试历史，
    /// 不再出现"后 start 清掉先 start 的 retry / end 挂错 retry"。
    retry_attempts: HashMap<(String, usize), Vec<RetryAttempt>>,
    /// 已收到 LlmCallEnd、但归属尚不可解析的完整终态。
    pending_terminal: HashMap<(String, usize), PendingTerminalState>,
    /// 每个 agent 当前活跃的 step：reasoning 分片只带 agent_id，需按此路由到自己的 generation。
    active_step_by_agent: HashMap<String, usize>,
}

impl GenerationTracker {
    pub(crate) fn new() -> Self {
        Self {
            generation_data: HashMap::new(),
            active_step: None,
            retry_attempts: HashMap::new(),
            pending_terminal: HashMap::new(),
            active_step_by_agent: HashMap::new(),
        }
    }

    pub(crate) fn on_llm_start(
        &mut self,
        agent_id: &str,
        step: usize,
        input: GenerationInput,
    ) -> GenerationStart {
        // 新 generation 的 retry 记录按 key 隔离，天然为空，无需清空全局 vec
        let gen_id = format!("gen_{}", uuid::Uuid::now_v7());
        let start_time = peri_time::now_utc_rfc3339();
        let cached = GenerationCached {
            gen_id: gen_id.clone(),
            start_time: start_time.clone(),
            completion_start_time: None,
            input,
        };
        self.generation_data
            .insert((agent_id.to_string(), step), cached);
        self.active_step_by_agent.insert(agent_id.to_string(), step);
        self.active_step = Some(step);
        GenerationStart { gen_id, start_time }
    }

    /// 记录首个推理分片时间（同一次 generation 只取最先到达的一次）。
    ///
    /// 按 agent 的活跃 step 精确路由，不按时间邻近猜测归属；该 agent 当前无活跃
    /// generation 时丢弃（例如分片晚于 LlmCallEnd 到达）。
    pub(crate) fn mark_completion_start(&mut self, agent_id: &str, at: String) {
        let Some(step) = self.active_step_by_agent.get(agent_id).copied() else {
            return;
        };
        if let Some(cached) = self.generation_data.get_mut(&(agent_id.to_string(), step)) {
            cached.completion_start_time.get_or_insert(at);
        }
    }

    /// 请求体到达：用实际请求来源替换 fallback，旧来源立即释放（ticket Drop 置取消位，
    /// 不等 IO）；`Rejected` 表示冻结被拒，同样释放 fallback 并显式标记。
    pub(crate) fn on_llm_request_payload(
        &mut self,
        agent_id: &str,
        step: usize,
        input: GenerationInput,
    ) {
        if let Some(cached) = self.generation_data.get_mut(&(agent_id.to_string(), step)) {
            cached.input = input;
        }
        // 未找到时静默 no-op（保留现有行为）
    }

    pub(crate) fn on_llm_retrying(
        &mut self,
        agent_id: &str,
        step: usize,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        _error: &str,
    ) {
        self.retry_attempts
            .entry((agent_id.to_string(), step))
            .or_default()
            .push(RetryAttempt {
                attempt,
                max_attempts,
                delay_ms,
            });
    }

    pub(crate) fn contains(&self, agent_id: &str, step: usize) -> bool {
        self.generation_data
            .contains_key(&(agent_id.to_string(), step))
    }

    pub(crate) fn preserve_terminal(
        &mut self,
        agent_id: &str,
        step: usize,
        terminal: PendingTerminalState,
    ) {
        self.pending_terminal
            .insert((agent_id.to_string(), step), terminal);
    }

    pub(crate) fn on_llm_end(&mut self, agent_id: &str, step: usize) -> Option<GenerationEnd> {
        let cached = self.generation_data.remove(&(agent_id.to_string(), step))?;
        if self.active_step_by_agent.get(agent_id) == Some(&step) {
            self.active_step_by_agent.remove(agent_id);
        }
        self.active_step = None;

        let retries = self.retry_attempts.remove(&(agent_id.to_string(), step));
        let retry_metadata = retries
            .filter(|r| !r.is_empty())
            .map(|r| build_retry_metadata(&r));

        let gen_id = cached.gen_id.clone();
        let start_time = cached.start_time.clone();
        let completion_start_time = cached.completion_start_time;
        let input = cached.input;

        Some(GenerationEnd {
            gen_id,
            start_time,
            completion_start_time,
            input,
            retry_metadata,
        })
    }

    pub(crate) fn take_all_active(&mut self) -> Vec<AbandonedGeneration> {
        self.active_step = None;
        self.active_step_by_agent.clear();
        self.generation_data
            .drain()
            .map(|((agent_id, step), cached)| {
                let retry_metadata = self
                    .retry_attempts
                    .remove(&(agent_id.clone(), step))
                    .filter(|retries| !retries.is_empty())
                    .map(|retries| build_retry_metadata(&retries));
                let gen_id = cached.gen_id.clone();
                let start_time = cached.start_time.clone();
                let completion_start_time = cached.completion_start_time;
                let input = cached.input;
                let terminal = self.pending_terminal.remove(&(agent_id.clone(), step));
                AbandonedGeneration {
                    agent_id,
                    step,
                    gen_id,
                    start_time,
                    completion_start_time,
                    input,
                    retry_metadata,
                    terminal,
                }
            })
            .collect()
    }

    pub(crate) fn active_step(&self) -> Option<usize> {
        self.active_step
    }
}

fn build_retry_metadata(retries: &[RetryAttempt]) -> serde_json::Value {
    serde_json::json!({
        "retry_count": retries.len(),
        "retries": retries.iter().map(|r| serde_json::json!({
            "attempt": r.attempt,
            "max_attempts": r.max_attempts,
            "delay_ms": r.delay_ms,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "generation_test.rs"]
mod tests;
