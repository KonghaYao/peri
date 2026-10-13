use thiserror::Error;

#[derive(Debug, Error)]
pub enum LangfuseError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON serialization failed: {0}")]
    JsonSerialize(#[from] serde_json::Error),

    #[error("Ingestion API returned errors: {0}")]
    IngestionApi(String),

    #[error("OTLP export partially rejected {rejected_spans} spans")]
    PartialSuccess { rejected_spans: u64 },

    #[error("batch queue is full")]
    QueueFull,

    #[error("telemetry payload exceeds the {limit_bytes}-byte budget")]
    PayloadTooLarge { limit_bytes: usize },

    #[error("Batch sender dropped, batcher is shut down")]
    ChannelClosed,

    /// The worker was joined, but exited by cancellation or panic rather than
    /// completing its drain. The panic payload is deliberately not exposed.
    #[error("Batch worker join failed (cancelled: {cancelled})")]
    WorkerJoinFailed { cancelled: bool },

    #[error("Invalid configuration: {0}")]
    Config(String),

    /// 暂存端口的分类错误（配额、IO、损坏、忙）；不含宿主路径与凭据。
    #[error("telemetry spool failed: {0}")]
    Spool(#[from] crate::spool::SpoolError),

    /// retained 内存预约不足；立即失败，不进入等待、不驱逐已受保护对象。
    #[error(
        "telemetry memory budget exceeded: requested {requested} bytes, available {available} bytes"
    )]
    MemoryBudgetExceeded { requested: usize, available: usize },

    /// 单个编码 span 已超过事件逻辑限额，无法通过拆批消化。
    #[error("single span exceeds the {limit_bytes}-byte event limit ({encoded_bytes} encoded)")]
    SingleSpanTooLarge {
        limit_bytes: usize,
        encoded_bytes: u64,
    },

    /// 预算身份不一致：金额相同不算同一预算，不允许重新挂账洗掉来源身份。
    /// 不携带地址与正文。
    #[error("telemetry budget identity mismatch")]
    BudgetMismatch,

    /// 仍有待交付事件，或 owner 尚未排空；**不是** HTTP 成功，也不能被 `is_ok` 当成完成。
    #[error("telemetry delivery still pending: {counts:?}")]
    Pending {
        counts: crate::batcher::DeliveryCounts,
    },

    /// 所有待交付事件均已在可恢复 Export 中，但仍在等待恢复发送；不是完成。
    #[error("telemetry delivery persisted but pending: {counts:?}")]
    PersistedPending {
        counts: crate::batcher::DeliveryCounts,
    },

    /// 已无待交付与活跃任务，但存在新增失败或待清理对象；不是完成。
    #[error("telemetry delivery incomplete: {counts:?}")]
    DeliveryIncomplete {
        counts: crate::batcher::DeliveryCounts,
    },

    /// flush 边界内无事件准备失败（与 `failed_events` 分开记账，不伪造事件失败）。
    #[error("{failed_snapshots} snapshot preparations failed: {counts:?}")]
    SnapshotPreparationFailed {
        failed_snapshots: u64,
        counts: crate::batcher::DeliveryCounts,
    },
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
