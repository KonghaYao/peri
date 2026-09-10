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

/// 部署退出结果；HTTP 失败和 worker 异常均不等同于成功发送。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LangfuseShutdownReport {
    Complete,
    DeliveryFailed { summary: String },
    WorkerFailed { cancelled: bool },
    UnexpectedFailure,
}

/// 新建部署独占的关闭权限。共享给 turn 的 SessionLike 不包含此权限。
/// 这里只保留原 session；Batcher 仍是唯一的 worker/join owner。
pub struct LangfuseShutdownOwner {
    session: Arc<LangfuseSession>,
}

impl LangfuseShutdownOwner {
    pub async fn shutdown(&self) -> LangfuseShutdownReport {
        self.session.shutdown().await
    }
}

impl LangfuseSession {
    /// 为新部署同时创建共享会话和不可克隆的关闭权限。
    pub async fn new_owned(
        config: LangfuseConfig,
        session_id: String,
    ) -> Option<(Arc<Self>, LangfuseShutdownOwner)> {
        let session = Arc::new(Self::new(config, session_id).await?);
        let owner = LangfuseShutdownOwner {
            session: Arc::clone(&session),
        };
        Some((session, owner))
    }

    /// 部署在全部生产者结束后关闭；取消等待保留原 Batcher join owner，允许重试。
    /// 此入口不属于 turn-facing LangfuseSessionLike。
    pub async fn shutdown(&self) -> LangfuseShutdownReport {
        match self.batcher.shutdown().await {
            Ok(()) => LangfuseShutdownReport::Complete,
            Err(LangfuseError::IngestionApi(summary)) => {
                LangfuseShutdownReport::DeliveryFailed { summary }
            }
            Err(LangfuseError::WorkerJoinFailed { cancelled }) => {
                LangfuseShutdownReport::WorkerFailed { cancelled }
            }
            Err(_) => LangfuseShutdownReport::UnexpectedFailure,
        }
    }

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
