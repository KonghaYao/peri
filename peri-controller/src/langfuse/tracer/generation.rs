//! LLM Generation 生命周期追踪器。
//!
//! 管理单次 LLM 调用从 start → end 的完整生命周期状态：
//! - on_llm_start：创建 GenerationCached，返回 GenerationStart
//! - on_llm_request_payload：补充 raw_body 字段
//! - on_llm_retrying：累积重试记录
//! - on_llm_end：取出缓存数据，返回 GenerationEnd 供外层构造 IngestionEvent

use std::collections::HashMap;
use std::sync::Arc;

use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

#[derive(Debug, Clone)]
pub(crate) struct GenerationCached {
    pub gen_id: String,
    pub start_time: String,
    pub messages_json: serde_json::Value,
    pub tools_json: serde_json::Value,
    pub raw_body: Option<Arc<serde_json::Value>>,
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
    pub input_json: serde_json::Value,
    pub retry_metadata: Option<serde_json::Value>,
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
}

impl GenerationTracker {
    pub(crate) fn new() -> Self {
        Self {
            generation_data: HashMap::new(),
            active_step: None,
            retry_attempts: HashMap::new(),
        }
    }

    pub(crate) fn on_llm_start(
        &mut self,
        agent_id: &str,
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    ) -> GenerationStart {
        // 新 generation 的 retry 记录按 key 隔离，天然为空，无需清空全局 vec
        let gen_id = format!("gen_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let cached = GenerationCached {
            gen_id: gen_id.clone(),
            start_time: start_time.clone(),
            messages_json: serde_json::to_value(&messages).unwrap_or_default(),
            tools_json: serde_json::to_value(&tools).unwrap_or_default(),
            raw_body: None,
        };
        self.generation_data
            .insert((agent_id.to_string(), step), cached);
        self.active_step = Some(step);
        GenerationStart { gen_id, start_time }
    }

    pub(crate) fn on_llm_request_payload(
        &mut self,
        agent_id: &str,
        step: usize,
        body: Arc<serde_json::Value>,
    ) {
        if let Some(cached) = self.generation_data.get_mut(&(agent_id.to_string(), step)) {
            cached.raw_body = Some(body);
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

    pub(crate) fn on_llm_end(&mut self, agent_id: &str, step: usize) -> Option<GenerationEnd> {
        let cached = self.generation_data.remove(&(agent_id.to_string(), step))?;
        self.active_step = None;

        let retries = self.retry_attempts.remove(&(agent_id.to_string(), step));
        let retry_metadata = retries
            .filter(|r| !r.is_empty())
            .map(|r| build_retry_metadata(&r));

        let input_json = cached
            .raw_body
            .map(|b| (*b).clone())
            .unwrap_or(cached.messages_json);

        Some(GenerationEnd {
            gen_id: cached.gen_id,
            start_time: cached.start_time,
            input_json,
            retry_metadata,
        })
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
