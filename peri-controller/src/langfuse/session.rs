use std::future::Future;
use std::pin::Pin;
use std::{sync::Arc, time::Duration};

use langfuse_client::{
    BackpressurePolicy, Batcher, BatcherConfig, IngestionEvent, LangfuseClient, LangfuseError,
};

use super::config::LangfuseConfig;
use super::drop_telemetry::LangfuseDropRegistry;
use super::session_like::LangfuseSessionLike;

/// Langfuse 进程级共享连接状态。
///
/// 生命周期：进程启动时构造一次，所有 session 的 `LangfuseTracer` 共享同一个 client + batcher。
/// `session_id` 标识进程级 session（per-turn 的 session_id 单独在 `LangfuseTracer` 级别传入）。
///
/// `config` 字段保存完整配置，供 LangfuseTracer 构造时读取采样等参数。
pub struct LangfuseSession {
    pub client: Arc<LangfuseClient>,
    pub batcher: Arc<Batcher>,
    pub drop_registry: LangfuseDropRegistry,
    pub session_id: String,
    pub config: LangfuseConfig,
}

impl LangfuseSession {
    /// 从配置构造 Session，失败时返回 None（静默降级）
    pub async fn new(config: LangfuseConfig, session_id: String) -> Option<Self> {
        let public_key = config.public_key.as_deref()?;
        let secret_key = config.secret_key.as_deref()?;

        let client = Arc::new(LangfuseClient::new(
            public_key,
            secret_key,
            &config.host,
            3, // max_retries
        ));

        let batcher_config = BatcherConfig {
            max_events: config.batch_max_events,
            flush_interval: Duration::from_secs(config.batch_flush_interval_secs),
            backpressure: BackpressurePolicy::DropNew,
            max_retries: 3,
        };
        let batcher = Batcher::new((*client).clone(), batcher_config);

        Some(Self {
            client,
            batcher: Arc::new(batcher),
            drop_registry: LangfuseDropRegistry::default(),
            session_id,
            config,
        })
    }
}

impl LangfuseSessionLike for LangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.batcher.try_add(event)
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = Result<(), LangfuseError>> + Send + '_>> {
        Box::pin(self.batcher.flush())
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }
}
