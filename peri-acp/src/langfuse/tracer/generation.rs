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
    pub error: String,
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
    generation_data: HashMap<usize, GenerationCached>,
    active_step: Option<usize>,
    retry_attempts: Vec<RetryAttempt>,
}

impl GenerationTracker {
    pub(crate) fn new() -> Self {
        Self {
            generation_data: HashMap::new(),
            active_step: None,
            retry_attempts: Vec::new(),
        }
    }

    pub(crate) fn on_llm_start(
        &mut self,
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    ) -> GenerationStart {
        // 清空 retry_attempts（新 step 开始）
        self.retry_attempts.clear();
        let gen_id = format!("gen_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let cached = GenerationCached {
            gen_id: gen_id.clone(),
            start_time: start_time.clone(),
            messages_json: serde_json::to_value(&messages).unwrap_or_default(),
            tools_json: serde_json::to_value(&tools).unwrap_or_default(),
            raw_body: None,
        };
        self.generation_data.insert(step, cached);
        self.active_step = Some(step);
        GenerationStart { gen_id, start_time }
    }

    pub(crate) fn on_llm_request_payload(&mut self, step: usize, body: Arc<serde_json::Value>) {
        if let Some(cached) = self.generation_data.get_mut(&step) {
            cached.raw_body = Some(body);
        }
        // 未找到时静默 no-op（保留现有行为）
    }

    pub(crate) fn on_llm_retrying(
        &mut self,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: &str,
    ) {
        self.retry_attempts.push(RetryAttempt {
            attempt,
            max_attempts,
            delay_ms,
            error: error.to_string(),
        });
    }

    pub(crate) fn on_llm_end(&mut self, step: usize) -> Option<GenerationEnd> {
        let cached = self.generation_data.remove(&step)?;
        self.active_step = None;

        let retry_metadata = if self.retry_attempts.is_empty() {
            None
        } else {
            Some(build_retry_metadata(&self.retry_attempts))
        };
        self.retry_attempts.clear();

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
            "error": r.error,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "generation_test.rs"]
mod tests;
