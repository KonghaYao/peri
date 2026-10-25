use super::*;
use peri_model::{ModelRequest, ProviderProtocol};
use serde_json::json;

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
