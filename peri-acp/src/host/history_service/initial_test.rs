//! History V2 `initial` 服务行为测试（H4a）。
//!
//! 全部使用 `FakeBackend` + `ManualClock`：覆盖单次 open、窗口复用、delivery 幂等/冲突/
//! 过期、容量、失败 effect 与占位清理。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::history_v2::{
    DecimalU64, ExecutionEffect, HistoryAnchor, HistoryBackendError, HistoryDirection,
    HistoryEffectState, HistoryErrorReason, HistoryInitialKind, HistoryInitialRequest,
    HistoryLimits, HistoryLiveBoundary, HistoryOpenedSnapshot, HistoryPageData, HistoryRecovery,
    HistoryV2Backend,
};
use serde_json::Value;

use super::backend_test::{
    big_page, body_params, limits, open_params, page_data, page_params, snap, tail_ref,
    FakeBackend, ManualClock,
};
use super::initial::HistoryInitialContext;
use super::ledger::LedgerLimits;
use super::service_test::{call, data, parse_page, reason, setup};
use super::state::HistoryClock;
use super::*;
use crate::host::live_delivery::LiveDelivery;

fn request(key: u64, limit: u32, target_bytes: u64) -> HistoryInitialRequest {
    HistoryInitialRequest {
        version: 2,
        request_key: DecimalU64::new(key),
        limit,
        target_bytes,
    }
}

fn initial_request(key: u64) -> HistoryInitialRequest {
    request(key, 50, 524288)
}

fn context(fake: &Arc<FakeBackend>, execution_ready: bool) -> HistoryInitialContext {
    HistoryInitialContext {
        session_id: "s1".to_owned(),
        backend: Some(fake.clone() as Arc<dyn HistoryV2Backend>),
        negotiated: true,
        limits: limits(),
        runtime_generation: "runtime-9".to_owned(),
        execution_ready,
        live: None,
        live_boundary: HistoryLiveBoundary {
            stream_id: "live-7".to_owned(),
            after_seq: DecimalU64::new(120),
        },
        reserved_response_bytes: 0,
    }
}

fn context_with_live(
    fake: &Arc<FakeBackend>,
    execution_ready: bool,
    live: &Arc<LiveDelivery>,
) -> HistoryInitialContext {
    let mut context = context(fake, execution_ready);
    context.live = Some(Arc::clone(live));
    context
}

fn snap_with(page: HistoryPageData) -> HistoryOpenedSnapshot {
    let mut opened = snap(7);
    opened.page = page;
    opened.page.history_revision = DecimalU64::new(7);
    opened.page.committed_tail = Some(tail_ref());
    opened.page.projection_version = 3;
    opened
}

fn snap_with_entries() -> HistoryOpenedSnapshot {
    snap_with(big_page(4, 3))
}

#[tokio::test]
async fn initial_uses_one_open_and_reuses_window_for_page() {
    let (service, _clock, fake) = setup(snap_with_entries());

    let result = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("initial succeeds");

    assert_eq!(result.version, 2);
    assert_eq!(result.kind, HistoryInitialKind::Initial);
    assert_eq!(result.request_key.get(), 1);
    assert!(result.execution_ready);
    assert_eq!(result.runtime_generation, "runtime-9");
    assert!(result.delivery_id.starts_with("dl_"));
    assert!(!result.delivery_id.contains(' '));
    assert_eq!(result.live_boundary.stream_id, "live-7");
    assert_eq!(result.live_boundary.after_seq.get(), 120);
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(result.page.history_revision.get(), 7);
    assert_eq!(
        result.page.committed_tail.as_ref().map(|tail| tail.get()),
        Some(42)
    );
    assert!(!result.page.window_id.is_empty());
    assert!(!result.page.snapshot_token.is_empty());
    assert!(!result.page.read_ticket.is_empty());

    {
        let calls = fake.open_calls.lock();
        assert_eq!(calls.len(), 1, "initial must not read twice");
        assert_eq!(calls[0].before, 50);
        assert_eq!(calls[0].after, 0);
        assert_eq!(calls[0].target_bytes, 524288);
        assert!(matches!(calls[0].anchor, HistoryAnchor::Latest));
        drop(calls);
        assert!(fake.page_calls.lock().is_empty());
    }
    assert_eq!(service.window_count(), 1);
    assert_eq!(service.opening_window_count(), 0);

    // 同一窗口/票据与 initial 页签发的 cursor 可继续翻页。
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    let value = service
        .handle(
            &page_params(
                2,
                "s1",
                &result.page.window_id,
                &result.page.snapshot_token,
                &result.page.read_ticket,
                result.page.before_cursor.as_deref(),
                "before",
                50,
            ),
            HistoryServiceRequest::new(
                true,
                Some(fake.clone() as Arc<dyn HistoryV2Backend>),
                limits(),
            ),
        )
        .await
        .expect("page continues from initial window");
    let page = parse_page(&value);
    assert_eq!(page.window_id, result.page.window_id);
    let calls = fake.page_calls.lock();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].raw_cursor, "b0");
    assert_eq!(calls[0].direction, HistoryDirection::Before);
}

#[tokio::test]
async fn initial_replays_same_bytes_and_rejects_conflicting_params() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let first = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("first");
    let replay = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("replay");
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&replay).unwrap()
    );
    assert_eq!(first.delivery_id, replay.delivery_id);
    assert_eq!(fake.open_calls.lock().len(), 1);
    assert_eq!(service.window_count(), 1);

    let error = service
        .initial(request(1, 49, 524288), context(&fake, true))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::RequestConflict);
    assert_eq!(fake.open_calls.lock().len(), 1);

    // 同一 requestKey 的 runtime 事实变化不能重放旧 executionReady=true。
    let mut changed = context(&fake, true);
    changed.runtime_generation = "runtime-10".to_owned();
    let error = service
        .initial(initial_request(1), changed)
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::DeliveryExpired);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Ready);
    assert_eq!(
        error_data.effect.runtime_generation.as_deref(),
        Some("runtime-10")
    );
    assert_eq!(fake.open_calls.lock().len(), 1);
}

#[tokio::test]
async fn initial_execute_begins_live_stream_and_replay_keeps_it() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let live = Arc::new(LiveDelivery::new());
    let mut first_context = context(&fake, true);
    first_context.live = Some(Arc::clone(&live));

    let first = service
        .initial(initial_request(1), first_context)
        .await
        .expect("first initial");
    assert_eq!(first.live_boundary.after_seq.get(), 0);
    assert!(live.has_stream_with_id("s1", &first.live_boundary.stream_id));
    assert!(live.is_paused("s1"), "首屏响应发送前保持暂停");

    // 同 requestKey 重放不换流：结果字节与流身份都保持不变。
    let replay = service
        .initial(initial_request(1), context_with_live(&fake, true, &live))
        .await
        .expect("replay");
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&replay).unwrap()
    );
    assert_eq!(
        replay.live_boundary.stream_id,
        first.live_boundary.stream_id
    );
    assert_eq!(fake.open_calls.lock().len(), 1, "重放不得重新装配首屏");
}

#[tokio::test]
async fn initial_failure_after_begin_leaves_no_paused_stream() {
    let (service, _clock, fake) = setup(snap_with_entries());
    fake.set_open_result(Err(HistoryBackendError::BackendUnavailable));
    let live = Arc::new(LiveDelivery::new());
    let mut failure_context = context(&fake, true);
    failure_context.live = Some(Arc::clone(&live));

    let _ = service
        .initial(initial_request(1), failure_context)
        .await
        .expect_err("后端不可用必须失败");
    assert!(
        !live.has_stream("s1"),
        "失败的 initial 不得遗留暂停流或投递身份"
    );

    // 失败后同键重试走全新执行，重新建立流。
    fake.set_open_result(Ok(snap_with_entries()));
    let retry = service
        .initial(initial_request(1), context_with_live(&fake, true, &live))
        .await
        .expect("重试成功");
    assert!(live.has_stream_with_id("s1", &retry.live_boundary.stream_id));
}

#[tokio::test]
async fn initial_delivery_expires_after_ledger_eviction() {
    let clock = Arc::new(ManualClock::new());
    let clock_ref: Arc<dyn HistoryClock> = clock.clone();
    let service = HistoryV2Service::with_ledger_limits(
        clock_ref,
        LedgerLimits {
            max_entries: 1,
            ..LedgerLimits::default()
        },
    );
    let fake = Arc::new(FakeBackend::new(snap_with_entries()));

    service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("first");
    service
        .initial(initial_request(2), context(&fake, true))
        .await
        .expect("second evicts first");
    // 同一会话的第二次 initial 替换旧窗口，不累积窗口。
    assert_eq!(service.window_count(), 1);

    let error = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::DeliveryExpired);
    assert_eq!(error_data.recovery, HistoryRecovery::Retry);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Ready);
    assert_eq!(
        error_data.effect.runtime_generation.as_deref(),
        Some("runtime-9")
    );
    assert_eq!(fake.open_calls.lock().len(), 2);
}

#[tokio::test]
async fn initial_delivery_expires_with_its_window() {
    let (service, clock, fake) = setup(snap_with_entries());
    service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("initial");

    clock.advance(Duration::from_secs(301));
    let error = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::DeliveryExpired);
    assert_eq!(service.window_count(), 0);
}

#[tokio::test]
async fn initial_replaces_same_session_windows_and_leaves_others_live() {
    let (service, _clock, fake) = setup(snap_with_entries());

    let first = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("first initial");
    let second = service
        .initial(initial_request(2), context(&fake, true))
        .await
        .expect("second initial replaces the first");
    assert_eq!(service.window_count(), 1, "same session keeps one window");

    // 旧窗口已关闭。
    let page = service
        .handle(
            &page_params(
                3,
                "s1",
                &first.page.window_id,
                &first.page.snapshot_token,
                &first.page.read_ticket,
                first.page.before_cursor.as_deref(),
                "before",
                50,
            ),
            HistoryServiceRequest::new(
                true,
                Some(fake.clone() as Arc<dyn HistoryV2Backend>),
                limits(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&page), HistoryErrorReason::WindowClosed);
    let body = service
        .handle(
            &body_params(
                4,
                "s1",
                &first.page.window_id,
                &first.page.snapshot_token,
                &first.page.read_ticket,
            ),
            HistoryServiceRequest::new(
                true,
                Some(fake.clone() as Arc<dyn HistoryV2Backend>),
                limits(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&body), HistoryErrorReason::WindowClosed);

    // 其他会话窗口不受影响。
    let other = call(&service, &fake, limits(), open_params(5, "s2", 50, 0))
        .await
        .expect("other session window");
    let other = parse_page(&other);
    service
        .initial(initial_request(6), context(&fake, true))
        .await
        .expect("third s1 initial");
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    service
        .handle(
            &page_params(
                7,
                "s2",
                &other.window_id,
                &other.snapshot_token,
                &other.read_ticket,
                other.before_cursor.as_deref(),
                "before",
                50,
            ),
            HistoryServiceRequest::new(
                true,
                Some(fake.clone() as Arc<dyn HistoryV2Backend>),
                limits(),
            ),
        )
        .await
        .expect("s2 window stays live");

    // 新窗口仍可读。
    assert_eq!(second.page.items.len(), 3);
    assert!(second.page.window_id != first.page.window_id);
}

#[tokio::test]
async fn initial_honours_window_capacity_and_failure_is_retryable() {
    let (service, clock, fake) = setup(snap_with_entries());
    // 用**其他会话**占满容量：同会话的 initial 会先替换自己的旧窗口。
    for key in 1..=8 {
        call(&service, &fake, limits(), open_params(key, "s2", 50, 0))
            .await
            .expect("fill capacity with another session");
    }

    let error = service
        .initial(initial_request(9), context(&fake, true))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::CapacityExceeded);
    assert_eq!(service.window_count(), 8);
    assert_eq!(service.opening_window_count(), 0);

    clock.advance(Duration::from_secs(301));
    let result = service
        .initial(initial_request(9), context(&fake, true))
        .await
        .expect("capacity failure retries after windows expire");
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(service.window_count(), 1);
}

#[tokio::test]
async fn initial_backend_failure_reports_effect_and_leaks_no_window() {
    let (service, _clock, fake) = setup(snap_with_entries());
    fake.set_open_result(Err(HistoryBackendError::PermissionDenied));

    let error = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32000);
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::PermissionDenied);
    assert_eq!(error_data.recovery, HistoryRecovery::None);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Ready);
    assert_eq!(error_data.effect.history, HistoryEffectState::NotPublished);
    assert_eq!(
        error_data.effect.runtime_generation.as_deref(),
        Some("runtime-9")
    );
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);

    // 失败记录可立即重试；同键复用，不等待 120 秒。
    fake.set_open_result(Ok(snap_with_entries()));
    let result = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("retry after backend failure");
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(service.window_count(), 1);
}

#[tokio::test]
async fn initial_failure_without_execution_ready_hides_runtime_generation() {
    let (service, _clock, fake) = setup(snap_with_entries());
    fake.set_open_result(Err(HistoryBackendError::BackendUnavailable));

    let error = service
        .initial(initial_request(1), context(&fake, false))
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::BackendUnavailable);
    assert_eq!(error_data.recovery, HistoryRecovery::Retry);
    assert_eq!(error_data.effect.execution, ExecutionEffect::NotReady);
    assert_eq!(error_data.effect.runtime_generation, None);
    assert_eq!(error_data.effect.history, HistoryEffectState::NotPublished);
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);
}

#[tokio::test]
async fn initial_without_execution_ready_still_returns_page() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let result = service
        .initial(initial_request(1), context(&fake, false))
        .await
        .expect("initial page is independent from execution readiness");

    assert!(!result.execution_ready);
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(result.runtime_generation, "runtime-9");
    assert_eq!(service.window_count(), 1);
}

#[tokio::test]
async fn initial_unnegotiated_and_missing_backend_are_domain_errors() {
    let (service, _clock, fake) = setup(snap_with_entries());

    let mut unnegotiated = context(&fake, true);
    unnegotiated.negotiated = false;
    let error = service
        .initial(initial_request(1), unnegotiated)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32000);
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::ProtocolNotNegotiated);
    assert_eq!(error_data.recovery, HistoryRecovery::Renegotiate);
    assert_eq!(error_data.effect.execution, ExecutionEffect::NotReady);
    assert!(fake.open_calls.lock().is_empty());
    assert_eq!(service.ledger_entry_count(), 0);
    assert_eq!(service.window_count(), 0);

    let mut missing = context(&fake, true);
    missing.backend = None;
    let error = service
        .initial(initial_request(2), missing)
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::BackendUnavailable);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Ready);
    assert_eq!(
        error_data.effect.runtime_generation.as_deref(),
        Some("runtime-9")
    );
    assert!(fake.open_calls.lock().is_empty());
    assert_eq!(service.ledger_entry_count(), 0);
}

#[tokio::test]
async fn initial_response_budget_failure_removes_window() {
    let (service, _clock, fake) = setup(snap_with(big_page(4096, 20)));
    let mut context = context(&fake, true);
    context.limits = HistoryLimits {
        target_page_bytes: 65536,
        max_response_bytes: 65536,
        ..limits()
    };

    let error = service
        .initial(request(1, 50, 65536), context)
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::BudgetExceeded);
    assert_eq!(error_data.recovery, HistoryRecovery::ReduceScope);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Ready);
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);
}

#[tokio::test]
async fn initial_response_budget_includes_outer_load_response_bytes() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let mut context = context(&fake, true);
    // 页面本身很小；外层 session/load 固定字段已占满预算时仍必须整体拒绝。
    context.reserved_response_bytes = context.limits.max_response_bytes;

    let error = service
        .initial(initial_request(1), context)
        .await
        .unwrap_err();
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::BudgetExceeded);
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);
}

#[tokio::test]
async fn cancelled_initial_releases_placeholder_and_allows_retry() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let _gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(initial_request(1), context(&task_fake, true))
            .await
    });
    fake.open_entered.notified().await;
    task.abort();
    assert!(task.await.is_err(), "aborted task reports cancellation");

    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);

    let result = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("retry after cancellation");
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(service.window_count(), 1);
}

#[tokio::test]
async fn initial_does_not_consume_key_when_params_are_invalid() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let error = service
        .initial(request(1, 0, 524288), context(&fake, true))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(service.ledger_entry_count(), 0);
    assert!(fake.open_calls.lock().is_empty());

    // 非法请求未占用 key，合法请求可继续使用同一个 requestKey。
    let result = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("valid request after invalid one");
    assert_eq!(result.page.items.len(), 3);
}

#[tokio::test]
async fn initial_fingerprint_binds_session() {
    let (service, _clock, fake) = setup(snap_with_entries());
    service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("initial");

    let mut other_session = context(&fake, true);
    other_session.session_id = "s2".to_owned();
    let error = service
        .initial(initial_request(1), other_session)
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::RequestConflict);
    assert_eq!(fake.open_calls.lock().len(), 1);
}

#[tokio::test]
async fn initial_result_serializes_under_expected_meta_shape() {
    let (service, _clock, fake) = setup(snap_with_entries());
    let result = service
        .initial(initial_request(1), context(&fake, true))
        .await
        .expect("initial");
    let value: Value = serde_json::to_value(&result).expect("serialize");
    assert_eq!(value["version"], 2);
    assert_eq!(value["kind"], "initial");
    assert_eq!(value["requestKey"], "1");
    assert_eq!(value["executionReady"], true);
    assert_eq!(value["runtimeGeneration"], "runtime-9");
    assert_eq!(value["liveBoundary"]["streamId"], "live-7");
    assert_eq!(value["liveBoundary"]["afterSeq"], "120");
    assert!(value["deliveryId"]
        .as_str()
        .is_some_and(|id| id.starts_with("dl_")));
    assert_eq!(value["page"]["kind"], "window");
    assert_eq!(value["page"]["items"].as_array().map(Vec::len), Some(3));
}
