use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::Notify;

use super::*;
use crate::batcher::split::OversizeReason;
use crate::memory::MemoryBudget;
use crate::payload::{BudgetedBytes, InputSnapshot};
use crate::snapshot::{SnapshotCell, SnapshotState, SnapshotTicket};
use crate::types::{IngestionEvent, TraceBody};

fn budget() -> Arc<MemoryBudget> {
    MemoryBudget::new(1 << 20).unwrap()
}

fn limits() -> BatchLimits {
    BatchLimits {
        max_event_bytes: 4096,
        max_batch_bytes: 8192,
        max_encoded_bytes: 8192,
        max_events: 10,
    }
}

fn event(input: Option<&str>) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: "trace".into(),
        timestamp: "2026-10-12T00:00:00Z".into(),
        body: TraceBody {
            id: Some("trace".into()),
            input: input.map(|text| serde_json::json!(text)),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 独立于被测代码的原文期望值：直接序列化事件对象。
fn expected_raw(event: &IngestionEvent) -> u64 {
    serde_json::to_vec(event).unwrap().len() as u64
}

/// 固定字节数的编码计量端口；调用次数可观察（用于断言未就绪事件不被计量）。
struct FixedMeter {
    bytes: u64,
    calls: Arc<AtomicUsize>,
}

impl OtlpMeter for FixedMeter {
    fn measure(&self, _events: &[TelemetryEvent], _limit: usize) -> Result<u64, LangfuseError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.bytes)
    }
}

fn fixed_meter(bytes: u64, calls: &Arc<AtomicUsize>) -> FixedMeter {
    FixedMeter {
        bytes,
        calls: Arc::clone(calls),
    }
}

/// 计量失败的端口：错误必须原样传播，不被吞成 0。
struct FailingMeter;

impl OtlpMeter for FailingMeter {
    fn measure(&self, _events: &[TelemetryEvent], _limit: usize) -> Result<u64, LangfuseError> {
        Err(LangfuseError::IngestionApi("meter failed".into()))
    }
}

#[test]
fn raw_and_encoded_are_measured_separately() {
    let budget = budget();
    let source = event(Some(&"x".repeat(300)));
    let telemetry = TelemetryEvent::try_new(source.clone(), None, None, &budget).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits(), &fixed_meter(4000, &calls)).unwrap();

    assert_eq!(
        prepared,
        PreparedEvent::Measured(EventMeasure {
            raw_bytes: expected_raw(&source),
            encoded_bytes: 4000,
        })
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn measurements_exactly_at_the_limits_are_not_oversized() {
    let budget = budget();
    let source = event(Some("boundary"));
    let telemetry = TelemetryEvent::try_new(source.clone(), None, None, &budget).unwrap();
    let raw = expected_raw(&source);
    let limits = BatchLimits {
        max_event_bytes: raw as usize,
        max_batch_bytes: raw as usize,
        max_encoded_bytes: 4096,
        max_events: 1,
    };
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits, &fixed_meter(4096, &calls)).unwrap();

    assert_eq!(
        prepared,
        PreparedEvent::Measured(EventMeasure {
            raw_bytes: raw,
            encoded_bytes: 4096,
        })
    );
}

#[test]
fn encoded_oversize_is_identified_with_the_full_measurement() {
    let budget = budget();
    let source = event(Some("small"));
    let telemetry = TelemetryEvent::try_new(source.clone(), None, None, &budget).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits(), &fixed_meter(9_999_999, &calls)).unwrap();

    match prepared {
        PreparedEvent::Oversized { measure, reason } => {
            // 完整计量：编码字节是真实值，不向下取整到限额。
            assert_eq!(measure.encoded_bytes, 9_999_999);
            assert_eq!(measure.raw_bytes, expected_raw(&source));
            assert_eq!(reason, OversizeReason::Encoded { limit_bytes: 8192 });
        }
        other => panic!("expected oversized, got {other:?}"),
    }
}

#[test]
fn raw_oversize_is_identified_without_truncating_the_event() {
    let budget = budget();
    let source = event(Some(&"y".repeat(300)));
    let telemetry = TelemetryEvent::try_new(source.clone(), None, None, &budget).unwrap();
    let limits = BatchLimits {
        max_event_bytes: 256,
        max_batch_bytes: 1024,
        max_encoded_bytes: 8192,
        max_events: 10,
    };
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits, &fixed_meter(10, &calls)).unwrap();

    match prepared {
        PreparedEvent::Oversized { measure, reason } => {
            assert_eq!(measure.raw_bytes, expected_raw(&source));
            assert!(measure.raw_bytes > 256);
            assert_eq!(measure.encoded_bytes, 10);
            assert_eq!(reason, OversizeReason::Raw { limit_bytes: 256 });
        }
        other => panic!("expected oversized, got {other:?}"),
    }
}

#[test]
fn preparing_snapshot_waits_without_measuring() {
    let budget = budget();
    let cell = Arc::new(SnapshotCell {
        budget: Arc::clone(&budget),
        prepare_seq: 1,
        state: Mutex::new(SnapshotState::Preparing),
        cancelled: AtomicBool::new(false),
        changed: Notify::new(),
    });
    let telemetry = TelemetryEvent::try_new(
        event(None),
        Some(InputSource::Preparing(SnapshotTicket { cell })),
        None,
        &budget,
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits(), &fixed_meter(1, &calls)).unwrap();

    assert_eq!(prepared, PreparedEvent::Waiting);
    // 未就绪来源不产生任何计量：不伪造「零内容」的最终编码。
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn ready_inline_source_keeps_the_raw_measurement_independent_of_content() {
    let budget = budget();
    let source = event(None);
    let permit = budget.try_reserve(64).unwrap();
    let bytes = BudgetedBytes::new(Bytes::from_static(br#"{"input":"payload"}"#), permit).unwrap();
    let telemetry = TelemetryEvent::try_new(
        source.clone(),
        Some(InputSource::Ready(InputSnapshot::Inline(bytes))),
        None,
        &budget,
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let prepared = prepare_event(&telemetry, &limits(), &fixed_meter(64, &calls)).unwrap();

    // 原文计量是冻结后的事件对象；来源正文由编码计量覆盖，不在这里重复累加。
    assert_eq!(
        prepared,
        PreparedEvent::Measured(EventMeasure {
            raw_bytes: expected_raw(&source),
            encoded_bytes: 64,
        })
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn meter_errors_propagate_unchanged() {
    let budget = budget();
    let telemetry = TelemetryEvent::try_new(event(Some("x")), None, None, &budget).unwrap();

    let result = prepare_event(&telemetry, &limits(), &FailingMeter);

    assert!(matches!(
        result,
        Err(LangfuseError::IngestionApi(message)) if message.contains("meter failed")
    ));
}

/// 生产接线形态：P2a 的 `measure_otlp` 是普通函数，必须能直接作为计量端口传入。
fn plain_meter(_events: &[TelemetryEvent], _limit: usize) -> Result<u64, LangfuseError> {
    Ok(777)
}

#[test]
fn plain_function_matches_the_otlp_meter_port() {
    let budget = budget();
    let telemetry = TelemetryEvent::try_new(event(Some("wiring")), None, None, &budget).unwrap();
    let meter: &dyn OtlpMeter = &plain_meter;

    let prepared = prepare_event(&telemetry, &limits(), meter).unwrap();

    assert!(matches!(
        prepared,
        PreparedEvent::Measured(EventMeasure {
            encoded_bytes: 777,
            ..
        })
    ));
}
