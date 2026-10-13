use std::future::Future;
use std::pin::Pin;
use std::{sync::Arc, time::Duration};

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{Compression, TelemetryConfig};
use langfuse_client::{
    BackpressurePolicy, Batcher, BatcherConfig, DeliveryCounts, FlushReport, IngestionEvent,
    InputSource, LangfuseClient, LangfuseError, MemoryBudget, ShutdownReport, SnapshotSource,
    SpoolStore, TelemetryEvent,
};

use peri_config::observability::LangfuseCompression;

use super::config::LangfuseConfig;
use super::drop_telemetry::LangfuseDropRegistry;
use super::session_like::LangfuseSessionLike;
use super::turn_traces::TurnTraceRegistry;

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
    /// sid → 活跃 turn trace；tracer 登记/清理，指标出口查表归属。
    pub turn_traces: TurnTraceRegistry,
}

/// 部署退出结果；HTTP 失败、待交付积压与 worker 异常均不等同于成功发送。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LangfuseShutdownReport {
    Complete,
    /// 仍有待交付事件，或 owner 尚未排空；关闭保持可重入。
    Pending {
        counts: DeliveryCounts,
    },
    /// 所有待交付事件都在可恢复 Export 中，仍等待恢复发送；不是完成。
    PersistedPending {
        counts: DeliveryCounts,
    },
    /// 已无待交付与活跃任务，但存在新增失败或待清理对象；不是完成。
    DeliveryIncomplete {
        counts: DeliveryCounts,
    },
    DeliveryFailed {
        summary: String,
    },
    WorkerFailed {
        cancelled: bool,
    },
    UnexpectedFailure,
}

impl LangfuseShutdownReport {
    /// 关闭事务是否仍持有本地任务/清理 owner（契约 §3.6、§5.2）。
    ///
    /// 为 `true` 时宿主必须保留原上下文重入关闭：报告是**未收敛**的观察，不是可缓存的终态。
    /// 判定不只看枚举名或 pending 计数：
    /// - `Pending`：仍待交付（含 owner 尚未排空），没有完成的移交；
    /// - `PersistedPending`：只有全部本地任务与清理 owner 都已收敛、且待交付事件全部登记为
    ///   不可撤回的持久积压（`persisted_pending_events == pending_events`）才算完成移交。
    ///
    /// 已移交的持久积压不能再被本进程推进（不轮询、不重发），因此可以缓存为终态；
    /// 其余终态规则不变。
    pub fn retains_ownership(&self) -> bool {
        match self {
            Self::Pending { .. } => true,
            Self::PersistedPending { counts } => {
                counts.active_tasks > 0
                    || counts.cleanup_pending_objects > 0
                    || counts.persisted_pending_events != counts.pending_events
            }
            _ => false,
        }
    }
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
        spool: Option<Arc<dyn SpoolStore>>,
    ) -> Option<(Arc<Self>, LangfuseShutdownOwner)> {
        let session = Arc::new(Self::new(config, session_id, spool).await?);
        let owner = LangfuseShutdownOwner {
            session: Arc::clone(&session),
        };
        Some((session, owner))
    }

    /// 部署在全部生产者结束后关闭；取消等待保留原 Batcher join owner，允许重试。
    /// 发送结果覆盖整个部署生命周期，包括此前已由 turn flush 观察的失败。
    /// 此入口不属于 turn-facing LangfuseSessionLike。
    ///
    /// 显式映射（契约 §3.6）：Pending/PersistedPending/DeliveryIncomplete 与无事件准备失败
    /// 一律按计数照实上报，绝不落 `UnexpectedFailure`，也不当作成功。
    pub async fn shutdown(&self) -> LangfuseShutdownReport {
        map_shutdown(self.batcher.shutdown().await)
    }

    /// 从配置构造 Session，失败时返回 None（静默降级）。
    ///
    /// `spool` 由部署装配层按 `spool_enabled` 决定是否提供（契约 §6/§7 P4b）；
    /// 运行期配置被拒时一律留下结构化诊断，不静默丢弃。
    pub async fn new(
        config: LangfuseConfig,
        session_id: String,
        spool: Option<Arc<dyn SpoolStore>>,
    ) -> Option<Self> {
        let public_key = config.public_key.as_deref()?;
        let secret_key = config.secret_key.as_deref()?;

        // 同一份 peri-config 投影只映射一次：client 与 batcher 共用 compression/max_wire_bytes，
        // 任一侧自行重解释会让 `Batcher::try_new` 在启动 worker 前拒绝这份部署（契约 §3.6）。
        let telemetry = telemetry_config(&config);
        let client = LangfuseClient::new(
            public_key,
            secret_key,
            &config.host,
            3, // max_retries：执行权由 client 独占，session 只传递
        )
        .with_export_config(ExportConfig {
            max_request_bytes: config.batch_max_bytes,
            compression: telemetry.compression,
            max_wire_bytes: telemetry.max_wire_bytes,
            ..Default::default()
        });
        let client = match client {
            Ok(client) => Arc::new(client),
            Err(error) => {
                tracing::warn!(%error, "Langfuse export configuration rejected");
                return None;
            }
        };

        let batcher_config = BatcherConfig {
            max_events: config.batch_max_events,
            queue_capacity: config.batch_queue_capacity,
            max_in_flight: config.batch_max_in_flight,
            max_event_bytes: config.batch_max_event_bytes,
            max_batch_bytes: config.batch_max_bytes,
            max_queue_bytes: config.batch_max_queue_bytes,
            flush_interval: Duration::from_secs(config.batch_flush_interval_secs),
            backpressure: BackpressurePolicy::DropNew,
            telemetry,
        };
        let batcher = match Batcher::try_new((*client).clone(), batcher_config, spool) {
            Ok(batcher) => batcher,
            Err(error) => {
                tracing::warn!(%error, "Langfuse batcher configuration rejected");
                return None;
            }
        };

        Some(Self {
            client,
            batcher: Arc::new(batcher),
            drop_registry: LangfuseDropRegistry::default(),
            session_id,
            config,
            turn_traces: TurnTraceRegistry::default(),
        })
    }
}

/// 部署终态 → 宿主可见关闭报告的显式映射（契约 §3.6）。
///
/// 独立成纯函数是因为 `Pending`/`PersistedPending`/`SnapshotPreparationFailed` 等终态无法由真实
/// 部署稳定制造，但每一条都必须有确定映射且绝不落 `UnexpectedFailure`。
fn map_shutdown(result: Result<ShutdownReport, LangfuseError>) -> LangfuseShutdownReport {
    match result {
        Ok(ShutdownReport::Complete) => LangfuseShutdownReport::Complete,
        Err(LangfuseError::Pending { counts }) => LangfuseShutdownReport::Pending { counts },
        Err(LangfuseError::PersistedPending { counts }) => {
            LangfuseShutdownReport::PersistedPending { counts }
        }
        Err(LangfuseError::DeliveryIncomplete { counts }) => {
            LangfuseShutdownReport::DeliveryIncomplete { counts }
        }
        // 准备失败没有事件级身份：以 DeliveryIncomplete 旁路报告其计数，
        // 不伪造 failed_events，也不升级成"意外失败"。
        Err(LangfuseError::SnapshotPreparationFailed { counts, .. }) => {
            LangfuseShutdownReport::DeliveryIncomplete { counts }
        }
        Err(LangfuseError::IngestionApi(summary)) => {
            LangfuseShutdownReport::DeliveryFailed { summary }
        }
        Err(LangfuseError::WorkerJoinFailed { cancelled }) => {
            LangfuseShutdownReport::WorkerFailed { cancelled }
        }
        Err(_) => LangfuseShutdownReport::UnexpectedFailure,
    }
}

/// peri-config 投影 → langfuse-client 遥测配置；逐字段直译，不解析来源、不夹带默认值。
fn telemetry_config(config: &LangfuseConfig) -> TelemetryConfig {
    TelemetryConfig {
        spool_enabled: config.spool_enabled,
        compression: compression(config.compression),
        input_spool_threshold_bytes: config.input_spool_threshold_bytes,
        payload_memory_threshold_bytes: config.payload_memory_threshold_bytes,
        memory_budget_bytes: config.memory_budget_bytes,
        prepare_max_in_flight: config.prepare_max_in_flight,
        stream_chunk_bytes: config.stream_chunk_bytes,
        spool_quota_bytes: config.spool_quota_bytes,
        spool_ttl: Duration::from_secs(config.spool_ttl_secs),
        max_wire_bytes: config.max_wire_bytes,
        shutdown_grace: Duration::from_secs(config.shutdown_grace_secs),
    }
}

/// 配置层枚举 → client 编码枚举的唯一映射点。
fn compression(value: LangfuseCompression) -> Compression {
    match value {
        LangfuseCompression::Gzip => Compression::Gzip,
        LangfuseCompression::None => Compression::None,
    }
}

impl LangfuseSessionLike for LangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.batcher.try_add(event)
    }

    fn try_freeze(&self, source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError> {
        self.batcher.try_freeze(source)
    }

    fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        self.batcher.try_add_prepared(event)
    }

    fn memory_budget(&self) -> &Arc<MemoryBudget> {
        self.batcher.memory_budget()
    }

    fn flush(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FlushReport, LangfuseError>> + Send + '_>> {
        Box::pin(self.batcher.flush())
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }

    fn turn_traces(&self) -> &TurnTraceRegistry {
        &self.turn_traces
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
