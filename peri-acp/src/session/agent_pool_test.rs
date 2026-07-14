use super::*;
use crate::provider::ThinkingConfig;

fn make_openai_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: model.to_string(),
        thinking: None,
    }
}

fn make_anthropic_provider(model: &str) -> LlmProvider {
    LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: model.to_string(),
        base_url: None,
        thinking: None,
    }
}

#[test]
fn test_agent_pool_new_is_empty() {
    let pool = AgentPool::new();
    assert!(pool.get_cached_llm().is_none());
    assert!(pool.fingerprint().is_empty());
}

#[test]
fn test_has_valid_cache_empty_pool() {
    let pool = AgentPool::new();
    let provider = make_openai_provider("gpt-4o");
    assert!(!pool.has_valid_cache(&provider));
}

#[test]
fn test_invalidate_clears_cache() {
    let mut pool = AgentPool::new();
    pool.fingerprint = "OpenAI:gpt-4o".to_string();
    pool.invalidate();
    assert!(pool.get_cached_llm().is_none());
    assert!(pool.fingerprint().is_empty());
}

#[test]
fn test_has_valid_cache_fingerprint_mismatch() {
    let mut pool = AgentPool::new();
    // 模拟已缓存但 fingerprint 不匹配
    pool.fingerprint = "OpenAI:gpt-4o".to_string();
    // cached_llm 为 None，has_valid_cache 应返回 false
    let provider = make_openai_provider("gpt-4o");
    assert!(!pool.has_valid_cache(&provider));
}

#[test]
fn test_fingerprint_openai() {
    let provider = make_openai_provider("gpt-4o-mini");
    let fp = fingerprint(&provider);
    assert_eq!(fp, "OpenAI:gpt-4o-mini");
}

#[test]
fn test_fingerprint_anthropic() {
    let provider = make_anthropic_provider("claude-sonnet-4-20250514");
    let fp = fingerprint(&provider);
    assert_eq!(fp, "Anthropic:claude-sonnet-4-20250514");
}

#[test]
fn test_has_valid_cache_after_fingerprint_only_set() {
    let mut pool = AgentPool::new();
    // 直接设置 fingerprint 但没有 cached_llm
    pool.fingerprint = "OpenAI:gpt-4o".to_string();
    let provider = make_openai_provider("gpt-4o");
    // cached_llm 为 None → false
    assert!(!pool.has_valid_cache(&provider));
}

#[test]
fn test_invalidate_clears_subagent_cache() {
    let mut pool = AgentPool::new();
    // 模拟 subagent_llm_cache 中有数据
    pool.subagent_llm_cache.insert(
        "OpenAI:gpt-4o".to_string(),
        Arc::new(mock_base_model("gpt-4o")),
    );
    assert!(!pool.subagent_llm_cache.is_empty());

    pool.invalidate();
    // invalidate 应同时清空 subagent_llm_cache
    assert!(pool.subagent_llm_cache.is_empty());
    assert!(pool.get_cached_llm().is_none());
    assert!(pool.fingerprint().is_empty());
}

#[test]
fn test_subagent_cache_miss_creates_new() {
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    // 首次查询 → 缓存未命中 → 创建新实例
    let model = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_base_model("gpt-4o"))
    });
    assert_eq!(model.model_id(), "gpt-4o");
    assert_eq!(model.provider_name(), "Mock");
}

#[test]
fn test_subagent_cache_hit_returns_same() {
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let m1 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_base_model("gpt-4o"))
    });
    let m2 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_base_model("gpt-4o"))
    });
    // 相同 fingerprint → 返回同一个 Arc（ptr_eq）
    assert!(Arc::ptr_eq(&m1, &m2));
}

#[test]
fn test_subagent_cache_different_fingerprint_isolation() {
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let m1 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_base_model("gpt-4o"))
    });
    let m2 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o-mini", || {
        Box::new(mock_base_model("gpt-4o-mini"))
    });
    assert_ne!(m1.model_id(), m2.model_id());
    assert!(!Arc::ptr_eq(&m1, &m2));
}

/// fingerprint 包含 thinking 维度，不同 effort 产生不同 fingerprint
#[test]
fn test_fingerprint_includes_thinking() {
    let provider_no_think = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        thinking: None,
    };
    let provider_low = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: "low".to_string(),
            max_tokens: 32000,
        }),
    };
    let provider_high = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: 16000,
            effort: "high".to_string(),
            max_tokens: 32000,
        }),
    };

    let fp_none = fingerprint(&provider_no_think);
    let fp_low = fingerprint(&provider_low);
    let fp_high = fingerprint(&provider_high);

    assert!(fp_none.contains("Anthropic:claude-sonnet-4-6"));
    assert!(
        !fp_none.contains(":think="),
        "无 thinking 时 fingerprint 不应含 :think="
    );
    assert!(
        fp_low.contains(":think=low:8000"),
        "thinking fingerprint 应包含 effort 和 budget"
    );
    assert!(fp_high.contains(":think=high:16000"));

    // 不同 effort 产生不同 fingerprint
    assert_ne!(fp_low, fp_high);
    // 不同 thinking 状态产生不同 fingerprint
    assert_ne!(fp_none, fp_low);
}

/// 同一 provider + model + thinking 配置产生相同 fingerprint
#[test]
fn test_fingerprint_same_thinking_stable() {
    let a = LlmProvider::Anthropic {
        api_key: "k1".into(), // api_key 不应影响 fingerprint
        model: "sonnet".into(),
        base_url: None,
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: "medium".to_string(),
            max_tokens: 32000,
        }),
    };
    let b = LlmProvider::Anthropic {
        api_key: "k2".into(), // 不同 api_key，fingerprint 应相同
        model: "sonnet".into(),
        base_url: Some("https://different.example.com".into()),
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: "medium".to_string(),
            max_tokens: 32000,
        }),
    };
    assert_eq!(fingerprint(&a), fingerprint(&b));
}

/// thinking.enabled=false 等同于无 thinking
#[test]
fn test_fingerprint_thinking_disabled_equals_none() {
    let none = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "sonnet".into(),
        base_url: None,
        thinking: None,
    };
    let disabled = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "sonnet".into(),
        base_url: None,
        thinking: Some(ThinkingConfig {
            enabled: false,
            budget_tokens: 8000,
            effort: "low".to_string(),
            max_tokens: 32000,
        }),
    };
    assert_eq!(fingerprint(&none), fingerprint(&disabled));
}

// 简单的 mock BaseModel 用于测试
fn mock_base_model(name: &str) -> impl BaseModel {
    use async_trait::async_trait;
    use peri_agent::{
        llm::{LlmRequest, LlmResponse, StopReason},
        messages::BaseMessage,
    };

    struct MockModel {
        name: String,
    }

    #[async_trait]
    impl BaseModel for MockModel {
        async fn invoke(
            &self,
            _request: LlmRequest,
        ) -> peri_agent::error::AgentResult<LlmResponse> {
            Ok(LlmResponse {
                message: BaseMessage::ai("mock response"),
                stop_reason: StopReason::EndTurn,
                usage: None,
                request_id: None,
            })
        }

        fn model_id(&self) -> &str {
            &self.name
        }

        fn provider_name(&self) -> &str {
            "Mock"
        }
    }

    MockModel {
        name: name.to_string(),
    }
}
