use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_agent::{
    llm::{types::LlmRequest, BaseModel},
    messages::BaseMessage,
};
use tokio::sync::Mutex as AsyncMutex;

/// 分类结果枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// 允许执行
    Allow,
    /// 拒绝执行
    Deny,
    /// 不确定，回退到人工审批
    Unsure,
}

/// 自动分类器 trait — 根据工具名称和输入判断是否放行
#[async_trait]
pub trait AutoClassifier: Send + Sync {
    async fn classify(&self, tool_name: &str, tool_input: &serde_json::Value) -> Classification;
}

// ─── 缓存条目 ────────────────────────────────────────────────────────────────

/// 缓存条目：存储分类结果和过期时间
struct CacheEntry {
    classification: Classification,
    expires_at: Instant,
}

// ─── LlmAutoClassifier ───────────────────────────────────────────────────────

/// 基于 LLM 的自动分类器实现
///
/// 持有 `Arc<AsyncMutex<Box<dyn BaseModel>>>` 调用 LLM 做分类，
/// 内置基于 `(tool_name, input_hash)` 的缓存，有效期 5 分钟。
pub struct LlmAutoClassifier {
    model: Arc<AsyncMutex<Box<dyn BaseModel>>>,
    cache: Mutex<HashMap<(String, u64), CacheEntry>>,
    cache_ttl: Duration,
}

impl LlmAutoClassifier {
    /// 创建新的 LLM 分类器
    pub fn new(model: Arc<AsyncMutex<Box<dyn BaseModel>>>) -> Self {
        Self {
            model,
            cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::from_secs(5 * 60),
        }
    }

    /// 使用自定义缓存 TTL 创建分类器
    pub fn with_cache_ttl(model: Arc<AsyncMutex<Box<dyn BaseModel>>>, cache_ttl: Duration) -> Self {
        Self {
            model,
            cache: Mutex::new(HashMap::new()),
            cache_ttl,
        }
    }

    /// 计算缓存键：(tool_name, input 的 64 位 hash)
    fn cache_key(tool_name: &str, tool_input: &serde_json::Value) -> (String, u64) {
        let input_str = serde_json::to_string(tool_input).unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        input_str.hash(&mut hasher);
        (tool_name.to_string(), hasher.finish())
    }

    /// 查询缓存，命中且未过期返回 Some，否则 None
    fn lookup_cache(&self, key: &(String, u64)) -> Option<Classification> {
        let cache = self.cache.lock();
        cache.get(key).and_then(|entry| {
            if entry.expires_at > Instant::now() {
                Some(entry.classification)
            } else {
                None
            }
        })
    }

    /// 写入缓存，同时淘汰过期条目
    fn insert_cache(&self, key: (String, u64), classification: Classification) {
        let mut cache = self.cache.lock();
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
        cache.insert(
            key,
            CacheEntry {
                classification,
                expires_at: now + self.cache_ttl,
            },
        );
    }

    /// 调用 LLM 进行分类
    async fn call_llm(&self, tool_name: &str, tool_input: &serde_json::Value) -> Classification {
        let prompt = format!(
            "Determine whether the following tool call is safe. Tool: {}, Input: {}\nAnswer with exactly one word: ALLOW, DENY, or UNSURE. Do not explain.",
            tool_name,
            serde_json::to_string(tool_input).unwrap_or_default(),
        );

        let request = LlmRequest::new(vec![BaseMessage::human(prompt)])
            .with_system("You are a tool call safety classifier. Respond with exactly one word: ALLOW, DENY, or UNSURE.")
            .with_max_tokens(32);

        let response = {
            let model = self.model.lock().await;
            model.invoke(request).await
        };

        match response {
            Ok(resp) => {
                let text = resp.message.content().trim().to_uppercase();
                // 提取所有纯字母单词
                let words: Vec<&str> = text
                    .split(|c: char| !c.is_alphabetic())
                    .filter(|w| !w.is_empty())
                    .collect();

                // 检查是否存在否定词（NOT, DON'T, WON'T, NEVER, etc.）
                let has_negation = words.iter().any(|w| {
                    matches!(
                        *w,
                        "NOT" | "DONT" | "WONT" | "CANT" | "NEVER" | "NO" | "NEITHER" | "NOR"
                    )
                });

                // 包含 DENY（无论有无否定）→ Deny；否定+ALLOW → Unsure
                if words.contains(&"DENY") {
                    Classification::Deny
                } else if words.contains(&"ALLOW") && !has_negation {
                    Classification::Allow
                } else {
                    Classification::Unsure
                }
            }
            Err(_) => Classification::Unsure,
        }
    }
}

#[async_trait]
impl AutoClassifier for LlmAutoClassifier {
    async fn classify(&self, tool_name: &str, tool_input: &serde_json::Value) -> Classification {
        let key = Self::cache_key(tool_name, tool_input);

        if let Some(cached) = self.lookup_cache(&key) {
            return cached;
        }

        let result = self.call_llm(tool_name, tool_input).await;

        self.insert_cache(key, result);

        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use peri_agent::{
        error::{AgentError, AgentResult},
        llm::types::{LlmResponse, StopReason},
    };

    use super::*;
    include!("auto_classifier_test.rs");
}
