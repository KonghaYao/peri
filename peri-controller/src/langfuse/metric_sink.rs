//! 指标事件的 Langfuse 出口。
//!
//! `peri-agent::metrics` 事件在此投影为 Langfuse **event** 观测（`EventCreate`），
//! 不占用 span / generation / trace 类型。
//!
//! 归属：指标自带 `sid`（会话 = thread id）时，优先挂到该 sid 当前活跃 turn 的
//! 根 span 下（见 [`TurnTraceRegistry`](crate::langfuse::TurnTraceRegistry)），
//! 不再为该 trace 制造第二个 root observation；只有无活跃登记（采样外 turn /
//! turn 已结束 / 指标无 sid）时才回退为独立 root trace，事件不丢。

use std::sync::Arc;

use langfuse_client::types::{EventBody, ObservationLevel};
use langfuse_client::IngestionEvent;
use peri_agent::metrics::{MetricEvent, MetricsSink};

use super::session_like::LangfuseSessionLike;
use super::tracer::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};

/// 指标事件的 Langfuse 出口：可用时上报，未配置 Langfuse 时不会被安装。
pub struct LangfuseMetricsSink {
    session: Arc<dyn LangfuseSessionLike>,
}

impl LangfuseMetricsSink {
    pub fn new(session: Arc<dyn LangfuseSessionLike>) -> Self {
        Self { session }
    }
}

impl MetricsSink for LangfuseMetricsSink {
    fn record(&self, event: MetricEvent) {
        // 有活跃 turn 登记时归属该 trace 并挂到其根 span 下（不再新开、不再成为
        // 第二个 root observation）；否则回退独立 root trace。
        let active = event
            .sid
            .as_deref()
            .and_then(|sid| self.session.turn_traces().resolve(sid));
        let (trace_id, parent_observation_id) = match active {
            Some(active) => (active.trace_id, active.parent_observation_id),
            None => (new_uuid(), None),
        };
        let body = EventBody {
            id: Some(new_uuid()),
            trace_id: Some(trace_id.clone()),
            name: Some(event.event.clone()),
            start_time: Some(event.ts.clone()),
            input: Some(event.data),
            output: None,
            metadata: Some(serde_json::json!({
                "event_type": "metric",
                "sid": event.sid,
                "rid": event.rid,
            })),
            level: Some(metric_level(&event.event)),
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id,
        };
        let ingestion_event = IngestionEvent::EventCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            ingestion_event,
            &trace_id,
            "metrics EventCreate",
        );
    }
}

/// `*.error` 指标按 Error 级别上报，其余保持 Default。
fn metric_level(event: &str) -> ObservationLevel {
    if event.ends_with(".error") {
        ObservationLevel::Error
    } else {
        ObservationLevel::Default
    }
}

#[cfg(test)]
#[path = "metric_sink_test.rs"]
mod tests;
