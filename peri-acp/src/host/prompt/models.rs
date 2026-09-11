//! Model factories share the captured provider configuration and the session pool.
use crate::{
    provider::{LlmProvider, PeriConfig},
    session::{
        agent_pool::{AgentPool, CachedLlmInstances},
        executor,
        retry_events::RetryEventForwarder,
    },
};
use std::sync::Arc;

type ModelFactory = Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>;
type CacheReader = Arc<dyn Fn() -> Option<CachedLlmInstances> + Send + Sync>;
type CacheWriter = Arc<dyn Fn(CachedLlmInstances) + Send + Sync>;

pub(super) struct ModelFactories {
    pub get_cached_llm: Option<CacheReader>,
    pub fresh_auxiliary_model: Option<ModelFactory>,
    pub store_llm: Option<CacheWriter>,
    pub primary_llm_factory: Option<ModelFactory>,
    pub auto_classifier_factory: Option<executor::AutoClassifierFactory>,
    pub subagent_llm_factory: Option<executor::SubagentLlmFactory>,
}

pub(super) fn build_model_factories(
    provider_snapshot: &LlmProvider,
    peri_config_snapshot: &Arc<PeriConfig>,
    pool: &Arc<parking_lot::Mutex<AgentPool>>,
    retry_events: &RetryEventForwarder,
    session_id: &str,
) -> ModelFactories {
    // 主 LLM 缓存读取（AgentPool has_valid_cache + get_cached_llm 语义）
    let get_cached_llm: Option<Arc<dyn Fn() -> Option<CachedLlmInstances> + Send + Sync>> = {
        let pool = Arc::clone(pool);
        let provider = provider_snapshot.clone();
        Some(Arc::new(move || {
            let guard = pool.lock();
            if guard.has_valid_cache(&provider) {
                guard.get_cached_llm().cloned()
            } else {
                None
            }
        }))
    };
    // fresh auxiliary model（缓存缺失时；retry observer 烘焙）
    let fresh_auxiliary_model: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(pool);
        let provider = provider_snapshot.clone();
        Some(Arc::new(move || {
            let provider = provider
                .clone()
                .with_retry_observer(Some(pool.lock().retry_events.as_retry_observer()));
            provider.into_model().into()
        }))
    };
    // LLM 缓存回写（AgentPool store_llm 语义）
    let store_llm: Option<Arc<dyn Fn(CachedLlmInstances) + Send + Sync>> = {
        let pool = Arc::clone(pool);
        Some(Arc::new(move |cache: CachedLlmInstances| {
            pool.lock().store_llm(cache);
        }))
    };
    // stage 装配 LLM 工厂（主 LLM / auto-classifier / 子 agent；与迁移前
    // stage_builder 桥内构造同源——AgentPool 缓存 + RetryObserver 烘焙）
    let primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(pool);
        let provider = provider_snapshot.clone();
        let retry_events = retry_events.clone();
        Some(Arc::new(move || {
            let fp = crate::session::agent_pool::fingerprint(&provider);
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(&pool, &fp, || {
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model()
            })
        }))
    };
    let auto_classifier_factory: Option<executor::AutoClassifierFactory> = {
        let provider = provider_snapshot.clone();
        let retry_events = retry_events.clone();
        Some(Arc::new(move || {
            Arc::new(tokio::sync::Mutex::new(
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model(),
            ))
        }))
    };
    let subagent_llm_factory: Option<executor::SubagentLlmFactory> = {
        let provider = provider_snapshot.clone();
        let peri_config = Arc::clone(peri_config_snapshot);
        let pool = Arc::clone(pool);
        let retry_events = retry_events.clone();
        let sid = session_id.to_owned();
        Some(Arc::new(move |model_alias: Option<&str>| {
            // 解析 provider 并构建 fingerprint
            let (p, fp) = if let Some(alias) = model_alias {
                match LlmProvider::from_config_for_alias(&peri_config, alias) {
                    Some(p) => {
                        let fp = crate::session::agent_pool::fingerprint(&p);
                        (Some(p), fp)
                    }
                    None => {
                        let fp = crate::session::agent_pool::fingerprint(&provider);
                        (None, fp)
                    }
                }
            } else {
                let fp = crate::session::agent_pool::fingerprint(&provider);
                (None, fp)
            };
            // 尝试 SubAgent 缓存
            let model: Arc<dyn peri_model::Model> =
                crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(
                    &pool,
                    &fp,
                    || match &p {
                        Some(p) => p
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                        None => provider
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                    },
                );
            let mut llm = peri_agent::agent::model_bridge::AgentModelBridge::from_arc(model);
            llm = llm.with_session_id(sid.clone());
            Box::new(llm)
        }))
    };

    ModelFactories {
        get_cached_llm,
        fresh_auxiliary_model,
        store_llm,
        primary_llm_factory,
        auto_classifier_factory,
        subagent_llm_factory,
    }
}
