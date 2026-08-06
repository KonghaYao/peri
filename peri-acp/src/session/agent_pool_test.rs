use super::*;
fn make_openai_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: model.to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    }
}

fn make_anthropic_provider(model: &str) -> LlmProvider {
    LlmProvider::Anthropic {
        api_key: "test-key".to_string(),
        model: model.to_string(),
        base_url: None,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
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
        Arc::new(mock_model("gpt-4o")) as Arc<dyn peri_model::Model>,
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
        Box::new(mock_model("gpt-4o"))
    });
    let prepared = model
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("mock prepare_request 可成功");
    assert_eq!(prepared.model_id(), "gpt-4o");
}

#[test]
fn test_subagent_cache_hit_returns_same() {
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let m1 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_model("gpt-4o"))
    });
    let m2 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_model("gpt-4o"))
    });
    // 相同 fingerprint → 返回同一个 Arc（ptr_eq）
    assert!(Arc::ptr_eq(&m1, &m2));
}

#[test]
fn test_subagent_cache_different_fingerprint_isolation() {
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let m1 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o", || {
        Box::new(mock_model("gpt-4o"))
    });
    let m2 = AgentPool::get_or_create_subagent_llm(&pool, "OpenAI:gpt-4o-mini", || {
        Box::new(mock_model("gpt-4o-mini"))
    });
    let id1 = m1
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("mock prepare_request 可成功")
        .model_id()
        .to_string();
    let id2 = m2
        .prepare_request(&peri_model::ModelRequest::default())
        .expect("mock prepare_request 可成功")
        .model_id()
        .to_string();
    assert_ne!(id1, id2);
    assert!(!Arc::ptr_eq(&m1, &m2));
}

/// fingerprint 包含 effort 维度，不同 effort 产生不同 fingerprint
#[test]
fn test_fingerprint_includes_effort() {
    let provider_no_effort = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    let provider_low = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        effort: Some("low".to_string()),
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    let provider_high = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "claude-sonnet-4-6".into(),
        base_url: None,
        effort: Some("high".to_string()),
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };

    let fp_none = fingerprint(&provider_no_effort);
    let fp_low = fingerprint(&provider_low);
    let fp_high = fingerprint(&provider_high);

    assert!(fp_none.contains("Anthropic:claude-sonnet-4-6"));
    assert!(
        !fp_none.contains(":effort="),
        "无 effort 时 fingerprint 不应含 :effort="
    );
    assert!(fp_low.contains(":effort=low"), "fingerprint 应包含 effort");
    assert!(fp_high.contains(":effort=high"));

    // 不同 effort 产生不同 fingerprint
    assert_ne!(fp_low, fp_high);
    // 不同 effort 状态产生不同 fingerprint
    assert_ne!(fp_none, fp_low);
}

/// 同一 provider + model + effort 配置产生相同 fingerprint
#[test]
fn test_fingerprint_same_effort_stable() {
    let a = LlmProvider::Anthropic {
        api_key: "k1".into(), // api_key 不应影响 fingerprint
        model: "sonnet".into(),
        base_url: None,
        effort: Some("medium".to_string()),
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    let b = LlmProvider::Anthropic {
        api_key: "k2".into(), // 不同 api_key，fingerprint 应相同
        model: "sonnet".into(),
        base_url: Some("https://different.example.com".into()),
        effort: Some("medium".to_string()),
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    assert_eq!(fingerprint(&a), fingerprint(&b));
}

/// 无 effort 等同于不启用 extended thinking
#[test]
fn test_fingerprint_no_effort_distinct() {
    let none = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "sonnet".into(),
        base_url: None,
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    let low = LlmProvider::Anthropic {
        api_key: "k".into(),
        model: "sonnet".into(),
        base_url: None,
        effort: Some("low".to_string()),
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    assert_ne!(fingerprint(&none), fingerprint(&low));
}

/// 跨 turn 新鲜度回归测试：转发器挂 AgentPool（session 级），
/// 池化模型烘焙的 observer 在 turn 2 仍能把事件送达 turn 2 的 handler。
///
/// turn 1 `set(handler1)` → 烘焙的 observer 收到事件；
/// turn 2 复用同一 pool（转发器不重建）`set(handler2)` →
/// 同一 observer（模拟缓存命中的池化模型）收到 turn 2 事件。
#[test]
fn test_retry_events_forwarder_survives_across_turns() {
    use peri_acp_types::event::{AgentEventHandler, ExecutorEvent, FnEventHandler};
    use std::sync::Mutex;

    fn capturing_handler(captured: &Arc<Mutex<Vec<String>>>) -> Arc<dyn AgentEventHandler> {
        let captured = Arc::clone(captured);
        Arc::new(FnEventHandler(move |event: ExecutorEvent| {
            if let ExecutorEvent::LlmRetrying { error, .. } = event {
                captured.lock().unwrap().push(error);
            }
        }))
    }

    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));

    // 模拟池化模型 create 时烘焙的 observer（缓存命中后跨 turn 复用同一 observer）。
    let observer = pool.lock().retry_events.as_retry_observer();

    // ── turn 1 ──
    let captured1: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    pool.lock()
        .retry_events
        .set(Some(capturing_handler(&captured1)));
    observer.on_retry(peri_model::RetryObservation::new(
        1,
        3,
        std::time::Duration::from_millis(500),
        peri_model::RetryErrorKind::Transport,
    ));
    assert_eq!(
        *captured1.lock().unwrap(),
        vec!["transport".to_string()],
        "turn 1: observer 应把事件送达 handler1"
    );

    // ── turn 2：同一 pool，覆盖式 set handler2（转发器不重建）──
    let captured2: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    pool.lock()
        .retry_events
        .set(Some(capturing_handler(&captured2)));
    observer.on_retry(peri_model::RetryObservation::new(
        2,
        3,
        std::time::Duration::from_millis(1000),
        peri_model::RetryErrorKind::Protocol,
    ));
    assert_eq!(
        *captured2.lock().unwrap(),
        vec!["protocol".to_string()],
        "turn 2: 同一 observer 应读到 handler2 并送达事件（跨 turn 新鲜度）"
    );
    // handler1 已被覆盖式替换，不应再收到 turn 2 事件（仍只有 turn 1 的事件）。
    assert_eq!(
        *captured1.lock().unwrap(),
        vec!["transport".to_string()],
        "turn 1 handler 不应收到 turn 2 事件"
    );
}

// 简单的 mock Model 用于测试
fn mock_model(name: &str) -> impl peri_model::Model {
    use async_trait::async_trait;
    use peri_model::{
        ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
        ModelStream, PreparedModelRequest, ProviderProtocol, StopReason,
    };
    use tokio_util::sync::CancellationToken;

    struct MockModel {
        name: String,
    }

    #[async_trait]
    impl peri_model::Model for MockModel {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                supports_tools: false,
                supports_reasoning: false,
                supports_vision: false,
                supports_streaming: true,
            }
        }

        fn prepare_request(&self, _request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
            PreparedModelRequest::observe(
                ProviderProtocol::Other {
                    value: "mock".to_string(),
                },
                self.name.clone(),
                url::Url::parse("https://mock.example/v1").expect("静态 URL"),
                serde_json::json!({}),
                std::collections::BTreeMap::new(),
            )
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelResult<ModelStream> {
            // 缓存身份测试只走 prepare_request，stream() 不应被调用
            Err(ModelError::cancelled())
        }

        async fn complete(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelResult<ModelResponse> {
            Ok(ModelResponse::new(
                ModelMessage::assistant_text("mock response"),
                StopReason::EndTurn,
                None,
                None,
            )?)
        }
    }

    MockModel {
        name: name.to_string(),
    }
}
