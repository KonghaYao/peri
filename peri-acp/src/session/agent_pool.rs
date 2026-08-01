//! Session-scoped agent component pool for reusing heavy objects across prompts.
//!
//! The biggest allocation win: `reqwest::Client` inside each LLM instance
//! is ~1-2 MB (connection pool + TLS session cache). Caching these across
//! prompts eliminates ~2-4 MB of transient allocation per turn.
//!
//! ### Cached entries
//! | Cache | Key | Entry | Lifetime |
//! |-------|-----|-------|----------|
//! | `cached_llm` | `"provider:model:think=effort:budget"` fingerprint | `auxiliary_model` + `auto_classifier_model` | Validated per-prompt via `has_valid_cache()` |
//! | `subagent_llm_cache` | `"provider:model:think=effort:budget"` fingerprint | `Arc<dyn Model>` (shared `reqwest::Client`) | Held until `invalidate()` or session close |

use std::{collections::HashMap, sync::Arc};

use crate::provider::LlmProvider;
use crate::session::retry_events::RetryEventForwarder;

/// Session-scoped cached LLM instances.
///
/// Contains `reqwest::Client` with connection pool + TLS session cache.
/// Reusing across prompts eliminates transient per-turn allocations.
#[derive(Clone)]
pub struct CachedLlmInstances {
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。
    /// Contains reqwest Client with connection pool.
    pub auxiliary_model: Arc<dyn peri_model::Model>,
    /// auto_classifier LLM (used by HITL HumanInTheLoopMiddleware).
    /// Contains a second reqwest Client.
    pub auto_classifier_model: Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>>,
    /// Provider fingerprint at time of creation (`"provider_name:model_name:think=effort:budget"`).
    pub fingerprint: String,
}

/// Session-scoped agent component pool.
///
/// Populated on first prompt, reused on subsequent prompts.
/// Invalidated when provider changes (model switch via `session/set_model`).
pub struct AgentPool {
    /// Cached LLM instances (biggest allocation win).
    cached_llm: Option<CachedLlmInstances>,
    /// Provider fingerprint for invalidation detection.
    fingerprint: String,
    /// SubAgent LLM cache: keyed by `"provider_name:model_name"` fingerprint.
    /// Each entry holds an `Arc<dyn Model>` with a shared `reqwest::Client`.
    /// Avoids creating a new HTTP client per SubAgent invocation.
    pub(crate) subagent_llm_cache: HashMap<String, Arc<dyn peri_model::Model>>,
    /// Session 级 retry 事件转发器（值字段，非 Arc）。
    ///
    /// 池化模型（subagent_llm_cache / cached_llm）跨 turn 存活时烘焙本转发器
    /// 的 observer；每 turn `build_agent` 覆盖式 `set` 当前 handler。
    /// `invalidate()` 不重置——转发器与 provider 缓存无关，跨 turn 观测状态应保留。
    pub(crate) retry_events: RetryEventForwarder,
}

impl Default for AgentPool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentPool {
    pub fn new() -> Self {
        Self {
            cached_llm: None,
            fingerprint: String::new(),
            subagent_llm_cache: HashMap::new(),
            retry_events: RetryEventForwarder::new(),
        }
    }

    /// Whether the cached LLM instances are valid for this provider.
    pub fn has_valid_cache(&self, provider: &LlmProvider) -> bool {
        let fp = fingerprint(provider);
        self.cached_llm.is_some() && self.fingerprint == fp
    }

    /// Store LLM instances after building.
    pub fn store_llm(&mut self, instances: CachedLlmInstances) {
        self.fingerprint = instances.fingerprint.clone();
        self.cached_llm = Some(instances);
    }

    /// Get cached LLM instances (returns `None` if cache empty or invalid).
    pub fn get_cached_llm(&self) -> Option<&CachedLlmInstances> {
        self.cached_llm.as_ref()
    }

    /// Invalidate cache (on model change, session clear, etc.).
    pub fn invalidate(&mut self) {
        self.cached_llm = None;
        self.fingerprint.clear();
        self.subagent_llm_cache.clear();
    }

    /// Current fingerprint (empty if no cache).
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Get or create a SubAgent LLM instance (double-checked locking).
    ///
    /// Fast path (cache hit): holds lock ~1μs to query HashMap.
    /// Slow path (cache miss): creates `reqwest::Client` outside lock (~10-100ms),
    /// then writes to cache inside lock, avoiding blocking other SubAgents' fast paths.
    pub(crate) fn get_or_create_subagent_llm(
        pool: &Arc<parking_lot::Mutex<AgentPool>>,
        fingerprint: &str,
        create: impl FnOnce() -> Box<dyn peri_model::Model>,
    ) -> Arc<dyn peri_model::Model> {
        // Fast path: query cache under lock
        {
            let guard = pool.lock();
            if let Some(cached) = guard.subagent_llm_cache.get(fingerprint) {
                return cached.clone();
            }
        }
        // Slow path: create outside lock
        let new_model: Arc<dyn peri_model::Model> = Arc::from(create());
        // Write back under lock (or_insert handles concurrent insert race)
        pool.lock()
            .subagent_llm_cache
            .entry(fingerprint.to_string())
            .or_insert(new_model)
            .clone()
    }
}

pub(crate) fn fingerprint(provider: &LlmProvider) -> String {
    format!(
        "{}:{}{}",
        provider.display_name(),
        provider.model_name(),
        provider.effort_key()
    )
}

#[cfg(test)]
#[path = "agent_pool_test.rs"]
mod tests;
