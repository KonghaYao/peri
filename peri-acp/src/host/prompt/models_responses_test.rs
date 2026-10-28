use super::*;
use peri_model::{ModelRequest, ProviderProtocol};
use serde_json::json;

#[test]
fn factories_cache_models_by_effective_capacity_for_every_provider() {
    for provider_type in ["anthropic", "openai", "openai_responses"] {
        let config: Arc<PeriConfig> = Arc::new(
            serde_json::from_value(json!({"config": {
                "providers":[{"id":"configured", "type":provider_type, "apiKey":"synthetic-key"}],
                "profiles":{
                    "opus":{"provider":"configured","model":"same-model","contextWindow":200000},
                    "haiku":{"provider":"configured","model":"same-model","contextWindow":64000}
                }
            }}))
            .unwrap(),
        );
        let large = LlmProvider::from_config_for_alias(&config, "opus").unwrap();
        let small = LlmProvider::from_config_for_alias(&config, "haiku").unwrap();
        let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
        let retry = pool.lock().retry_events.clone();
        let factories = build_model_factories(&large, &config, &pool, &retry, "fixture-session");
        let primary = factories.primary_llm_factory.as_ref().unwrap()();
        assert_eq!(primary.context_window(), Some(200_000));
        assert!(Arc::ptr_eq(
            &primary,
            &factories.primary_llm_factory.as_ref().unwrap()()
        ));

        let child_factory = factories.subagent_llm_factory.as_ref().unwrap();
        child_factory(Some("haiku"));
        let small_key = crate::session::agent_pool::fingerprint(&small);
        let large_key = crate::session::agent_pool::fingerprint(&large);
        assert_ne!(large_key, small_key, "{provider_type}");
        let child = pool.lock().subagent_llm_cache[&small_key].clone();
        assert_eq!(child.context_window(), Some(64_000));
        assert!(!Arc::ptr_eq(&primary, &child));
        child_factory(Some("haiku"));
        assert!(Arc::ptr_eq(
            &child,
            &pool.lock().subagent_llm_cache[&small_key]
        ));
        child_factory(Some("opus"));
        assert!(Arc::ptr_eq(
            &primary,
            &pool.lock().subagent_llm_cache[&large_key]
        ));

        let auxiliary = factories.fresh_auxiliary_model.as_ref().unwrap()();
        let classifier = factories.auto_classifier_factory.as_ref().unwrap()();
        factories.store_llm.as_ref().unwrap()(CachedLlmInstances {
            auxiliary_model: auxiliary.clone(),
            auto_classifier_model: classifier,
            fingerprint: large_key,
        });
        let cached = factories.get_cached_llm.as_ref().unwrap()().unwrap();
        assert!(Arc::ptr_eq(&auxiliary, &cached.auxiliary_model));
        let small_factories =
            build_model_factories(&small, &config, &pool, &retry, "fixture-session");
        assert!(small_factories.get_cached_llm.as_ref().unwrap()().is_none());
        let small_primary = small_factories.primary_llm_factory.as_ref().unwrap()();
        assert!(Arc::ptr_eq(&child, &small_primary));
        assert_eq!(small_primary.context_window(), Some(64_000));
    }
}

#[test]
fn test_responses_factories_after_config_roundtrip_cover_primary_compact_and_subagent() {
    let config: PeriConfig = serde_json::from_value(json!({"config": {
        "providers":[{"id":"responses", "type":"openai_responses", "apiKey":"fixture-key"}],
        "profiles":{
            "opus":{"provider":"responses","model":"main-model","effort":"high","contextWindow":120000},
            "haiku":{"provider":"responses","model":"small-model","effort":"low","contextWindow":64000}
        }
    }})).unwrap();
    // The same frozen configuration is reconstructed when a host/session is recreated.
    let config: Arc<PeriConfig> =
        Arc::new(serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap());
    let provider = LlmProvider::from_config_for_alias(&config, "opus").unwrap();
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let retry = pool.lock().retry_events.clone();
    let factories = build_model_factories(&provider, &config, &pool, &retry, "fixture-session");
    for model in [
        factories.primary_llm_factory.as_ref().unwrap()(),
        factories.fresh_auxiliary_model.as_ref().unwrap()(),
    ] {
        let prepared = model.prepare_request(&ModelRequest::default()).unwrap();
        assert_eq!(prepared.protocol(), &ProviderProtocol::OpenAiResponses);
        assert_eq!(prepared.model_id(), "main-model");
        assert_eq!(model.context_window(), Some(120000));
    }
    let source = factories.subagent_llm_factory.as_ref().unwrap()(Some("haiku"));
    assert_eq!(source.model_name(), "small-model");
    let child = LlmProvider::from_config_for_alias(&config, "haiku").unwrap();
    let child_key = crate::session::agent_pool::fingerprint(&child);
    let cached = pool.lock().subagent_llm_cache[&child_key].clone();
    let prepared = cached.prepare_request(&ModelRequest::default()).unwrap();
    assert_eq!(prepared.model_id(), "small-model");
    assert_eq!(prepared.body().as_value()["reasoning"]["effort"], "low");
    assert_eq!(cached.context_window(), Some(64000));
    factories.subagent_llm_factory.as_ref().unwrap()(Some("haiku"));
    assert!(Arc::ptr_eq(
        &cached,
        &pool.lock().subagent_llm_cache[&child_key]
    ));
}
