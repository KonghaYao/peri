//! History V2 连接级服务行为测试（H2）。
//!
//! 全部使用 `FakeBackend` + `ManualClock`：不读 SQLite，不依赖真实时间。覆盖窗口生命周期、
//! 幂等账本、cursor 绑定、预算、并发关闭与错误映射。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use peri_acp_types::history_v2::{
    ExecutionEffect, HistoryAnchor, HistoryClosedResult, HistoryDirection, HistoryErrorData,
    HistoryErrorReason, HistoryLimits, HistoryRecovery, HistoryWindowPage,
};

use super::backend_test::{
    big_page, body_params, close_params, limits, open_entry_params, open_params, page_data,
    page_params, seek_params, snap, FakeBackend, ManualClock,
};
use super::ledger::LedgerLimits;
use super::state::HistoryClock;
use super::*;

type TestResult = Result<Value, AcpError>;

pub(super) fn setup(
    open: peri_acp_types::history_v2::HistoryOpenedSnapshot,
) -> (Arc<HistoryV2Service>, Arc<ManualClock>, Arc<FakeBackend>) {
    let clock = Arc::new(ManualClock::new());
    let clock_ref: Arc<dyn HistoryClock> = clock.clone();
    let service = Arc::new(HistoryV2Service::with_ledger_limits(
        clock_ref,
        LedgerLimits::default(),
    ));
    (service, clock, Arc::new(FakeBackend::new(open)))
}

fn backend(fake: &Arc<FakeBackend>) -> Arc<dyn peri_acp_types::history_v2::HistoryV2Backend> {
    fake.clone()
}

pub(super) async fn call(
    service: &HistoryV2Service,
    fake: &Arc<FakeBackend>,
    limits: HistoryLimits,
    params: Value,
) -> TestResult {
    service
        .handle(
            &params,
            HistoryServiceRequest::new(true, Some(backend(fake)), limits),
        )
        .await
}

async fn unnegotiated(service: &HistoryV2Service, fake: &Arc<FakeBackend>) -> TestResult {
    service
        .handle(
            &open_params(1, "s1", 50, 0),
            HistoryServiceRequest::new(false, Some(backend(fake)), limits()),
        )
        .await
}

pub(super) fn parse_page(value: &Value) -> HistoryWindowPage {
    serde_json::from_value(value.clone()).expect("window page")
}

/// 断言服务层 cursor 形状：`v2|{32 hex mac}|{window}|{direction}|{raw}`。
pub(super) fn assert_cursor(cursor: &Option<String>, window: &str, direction: &str, raw: &str) {
    let cursor = cursor.as_deref().expect("cursor");
    let parts: Vec<&str> = cursor.splitn(5, '|').collect();
    assert_eq!(parts.len(), 5, "cursor shape: {cursor}");
    assert_eq!(parts[0], "v2");
    assert_eq!(parts[1].len(), 32, "mac must be 16 bytes hex");
    assert!(
        parts[1].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "mac must be hex"
    );
    assert_eq!(parts[2], window);
    assert_eq!(parts[3], direction);
    assert_eq!(parts[4], raw);
}

fn parse_closed(value: &Value) -> HistoryClosedResult {
    serde_json::from_value(value.clone()).expect("closed result")
}

pub(super) fn data(error: &AcpError) -> HistoryErrorData {
    serde_json::from_value(error.data.clone().expect("domain data")).expect("history error data")
}

pub(super) fn reason(error: &AcpError) -> HistoryErrorReason {
    data(error).reason
}

pub(super) struct Window {
    pub(super) id: String,
    pub(super) token: String,
    pub(super) ticket: String,
}

pub(super) fn window_of(page: &HistoryWindowPage) -> Window {
    Window {
        id: page.window_id.clone(),
        token: page.snapshot_token.clone(),
        ticket: page.read_ticket.clone(),
    }
}

pub(super) async fn open_ok(
    service: &HistoryV2Service,
    fake: &Arc<FakeBackend>,
    key: u64,
) -> (HistoryWindowPage, Window) {
    let value = call(service, fake, limits(), open_params(key, "s1", 50, 0))
        .await
        .expect("open succeeds");
    let page = parse_page(&value);
    let window = window_of(&page);
    (page, window)
}

#[tokio::test]
async fn open_latest_returns_tokens_wrapped_cursors_and_backend_plan() {
    let (service, _clock, fake) = setup(snap(7));
    let (page, window) = open_ok(&service, &fake, 1).await;

    assert!(!window.id.is_empty());
    assert!(!window.token.is_empty());
    assert!(!window.ticket.is_empty());
    assert_eq!(page.history_revision.get(), 7);
    assert_eq!(page.committed_tail.map(|tail| tail.get()), Some(42));
    assert_cursor(&page.before_cursor, &window.id, "before", "b0");
    assert_cursor(&page.after_cursor, &window.id, "after", "a0");

    let calls = fake.open_calls.lock();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].session_id, "s1");
    assert_eq!(calls[0].anchor, HistoryAnchor::Latest);
    assert_eq!(calls[0].before, 50);
    assert_eq!(calls[0].after, 0);
    assert_eq!(calls[0].target_bytes, 524288);
}

#[tokio::test]
async fn open_backend_failure_maps_reason_and_registers_no_window() {
    let (service, _clock, fake) = setup(snap(7));
    fake.set_open_result(Err(
        peri_acp_types::history_v2::HistoryBackendError::PermissionDenied,
    ));
    let error = call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32000);
    assert_eq!(reason(&error), HistoryErrorReason::PermissionDenied);
    assert_eq!(data(&error).recovery, HistoryRecovery::None);
    assert_eq!(service.window_count(), 0);
}

#[tokio::test]
async fn page_without_cursor_uses_home_and_does_not_move_it() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));

    let first = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect("page from home");
    let first = parse_page(&first);
    assert_cursor(&first.before_cursor, &window.id, "before", "b1");

    let second = call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect("second page from same home");
    assert!(parse_page(&second).items.is_empty());

    let calls = fake.page_calls.lock();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].raw_cursor, "b0");
    assert_eq!(calls[1].raw_cursor, "b0");
    assert_eq!(calls[0].direction, HistoryDirection::Before);
    assert_eq!(calls[0].limit, 50);
}

#[tokio::test]
async fn page_direction_mismatch_is_invalid_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (page, window) = open_ok(&service, &fake, 1).await;
    let cursor = page.before_cursor.clone().expect("before cursor");

    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            Some(&cursor),
            "after",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
    assert!(fake.page_calls.lock().is_empty());
}

#[tokio::test]
async fn page_cursor_from_other_window_is_cross_window_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (_first, first) = open_ok(&service, &fake, 1).await;
    let (second_page, _second) = open_ok(&service, &fake, 2).await;
    let foreign = second_page.before_cursor.clone().expect("before cursor");

    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &first.id,
            &first.token,
            &first.ticket,
            Some(&foreign),
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::CrossWindowCursor);
}

#[tokio::test]
async fn page_bare_raw_cursor_is_invalid_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            Some("b0"),
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
}

#[tokio::test]
async fn page_wrong_ticket_is_ticket_invalid() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            "wrong",
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::TicketInvalid);
}

#[tokio::test]
async fn page_wrong_snapshot_token_is_snapshot_invalidated() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            "wrong",
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::SnapshotInvalidated);
}

#[tokio::test]
async fn page_session_mismatch_is_permission_denied() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "other",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::PermissionDenied);
}

#[tokio::test]
async fn close_then_page_is_window_closed_and_close_replays() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let close = close_params(2, "s1", &window.id, &window.token, &window.ticket);
    let closed = call(&service, &fake, limits(), close.clone())
        .await
        .expect("close succeeds");
    assert_eq!(parse_closed(&closed).window_id, window.id);

    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowClosed);

    let replay = call(&service, &fake, limits(), close)
        .await
        .expect("close replays");
    assert_eq!(
        serde_json::to_vec(&closed).unwrap(),
        serde_json::to_vec(&replay).unwrap()
    );
}

#[tokio::test]
async fn idle_window_expires_after_five_minutes() {
    let (service, clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    clock.advance(Duration::from_secs(301));
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowExpired);
}

#[tokio::test]
async fn absolute_window_expiry_wins_over_recent_use() {
    let (service, clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    fake.set_page_result(Ok(page_data(7, Some("b0"), Some("a0"))));
    for key in 2..=8 {
        clock.advance(Duration::from_secs(250));
        call(
            &service,
            &fake,
            limits(),
            page_params(
                key,
                "s1",
                &window.id,
                &window.token,
                &window.ticket,
                None,
                "before",
                50,
            ),
        )
        .await
        .expect("recent use keeps the window alive");
    }
    // 总时长 2000s > 1800s 绝对期限；最后一次使用仅过去 250s。
    clock.advance(Duration::from_secs(250));
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            9,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowExpired);
}

#[tokio::test]
async fn opening_placeholder_is_pruned_after_sixty_seconds() {
    let (service, clock, fake) = setup(snap(7));
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task = tokio::spawn(async move {
        call(
            &task_service,
            &task_fake,
            limits(),
            open_params(1, "s1", 50, 0),
        )
        .await
    });
    fake.open_entered.notified().await;
    clock.advance(Duration::from_secs(61));
    gate.notify_one();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowExpired);
    assert_eq!(service.window_count(), 0);
}

#[tokio::test]
async fn capacity_rejects_ninth_window_and_expiry_frees_a_slot() {
    let (service, clock, fake) = setup(snap(7));
    for key in 1..=8 {
        open_ok(&service, &fake, key).await;
    }
    let error = call(&service, &fake, limits(), open_params(9, "s1", 50, 0))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::CapacityExceeded);
    assert_eq!(service.window_count(), 8);

    clock.advance(Duration::from_secs(301));
    open_ok(&service, &fake, 10).await;
}

#[tokio::test]
async fn same_key_same_fingerprint_replays_success() {
    let (service, _clock, fake) = setup(snap(7));
    let params = open_params(1, "s1", 50, 0);
    let first = call(&service, &fake, limits(), params.clone())
        .await
        .expect("first");
    let replay = call(&service, &fake, limits(), params)
        .await
        .expect("replay");
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&replay).unwrap()
    );
    assert_eq!(fake.open_calls.lock().len(), 1);
}

#[tokio::test]
async fn same_key_different_fingerprint_is_request_conflict() {
    let (service, _clock, fake) = setup(snap(7));
    open_ok(&service, &fake, 1).await;
    let error = call(&service, &fake, limits(), open_params(1, "s1", 49, 0))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::RequestConflict);
}

#[tokio::test]
async fn concurrent_same_key_returns_operation_in_progress() {
    let (service, _clock, fake) = setup(snap(7));
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task = tokio::spawn(async move {
        call(
            &task_service,
            &task_fake,
            limits(),
            open_params(1, "s1", 50, 0),
        )
        .await
    });
    fake.open_entered.notified().await;
    let error = call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::OperationInProgress);
    gate.notify_one();
    task.await.unwrap().expect("owner succeeds");
}

#[tokio::test]
async fn gap_then_old_key_is_operation_expired() {
    let (service, _clock, fake) = setup(snap(7));
    open_ok(&service, &fake, 5).await;
    let error = call(&service, &fake, limits(), open_params(3, "s1", 50, 0))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::OperationExpired);
}

#[tokio::test]
async fn in_progress_can_be_retried_after_120_seconds() {
    let (service, clock, fake) = setup(snap(7));
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task = tokio::spawn(async move {
        call(
            &task_service,
            &task_fake,
            limits(),
            open_params(1, "s1", 50, 0),
        )
        .await
    });
    fake.open_entered.notified().await;
    let in_progress = call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .unwrap_err();
    assert_eq!(
        reason(&in_progress),
        HistoryErrorReason::OperationInProgress
    );

    clock.advance(Duration::from_secs(121));
    call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .expect("timed-out operation can be retried");

    gate.notify_one();
    let stale = task.await.unwrap().unwrap_err();
    assert_eq!(reason(&stale), HistoryErrorReason::WindowExpired);
    assert_eq!(service.window_count(), 1);
}

#[tokio::test]
async fn ledger_byte_budget_evicts_success_and_retry_expires() {
    let clock = Arc::new(ManualClock::new());
    let clock_ref: Arc<dyn HistoryClock> = clock.clone();
    let service = Arc::new(HistoryV2Service::with_ledger_limits(
        clock_ref,
        LedgerLimits {
            max_bytes: 64,
            ..LedgerLimits::default()
        },
    ));
    let fake = Arc::new(FakeBackend::new(snap(7)));
    let params = open_params(1, "s1", 50, 0);
    call(&service, &fake, limits(), params.clone())
        .await
        .expect("open succeeds but response exceeds ledger byte budget");
    assert_eq!(service.window_count(), 1);
    assert_eq!(service.ledger_entry_count(), 0);

    let error = call(&service, &fake, limits(), params).await.unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::OperationExpired);
}

#[tokio::test]
async fn backend_revision_change_invalidates_and_removes_window() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    fake.set_page_result(Ok(page_data(8, Some("b1"), Some("a1"))));
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::SnapshotInvalidated);
    assert_eq!(service.window_count(), 0);
}

#[tokio::test]
async fn backend_committed_tail_change_invalidates_and_removes_window() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let mut changed_tail = page_data(7, Some("b1"), Some("a1"));
    changed_tail.committed_tail = Some(peri_acp_types::history_v2::HistoryCommittedTailRef {
        entry_id: "entry-43".to_owned(),
        position: 43,
    });
    fake.set_page_result(Ok(changed_tail));
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::SnapshotInvalidated);
    assert_eq!(service.window_count(), 0);
}

#[tokio::test]
async fn body_invalid_offset_and_zero_max_are_invalid_params() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;

    let mut bad_offset = body_params(2, "s1", &window.id, &window.token, &window.ticket);
    bad_offset["offset"] = json!("-1");
    let error = call(&service, &fake, limits(), bad_offset)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());

    let mut zero = body_params(3, "s1", &window.id, &window.token, &window.ticket);
    zero["maxBytes"] = json!(0);
    let error = call(&service, &fake, limits(), zero).await.unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());
}

#[tokio::test]
async fn body_over_limit_is_invalid_params() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let mut params = body_params(2, "s1", &window.id, &window.token, &window.ticket);
    params["maxBytes"] = json!(999_999_u64);
    let error = call(&service, &fake, limits(), params).await.unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());
}

#[tokio::test]
async fn unnegotiated_and_missing_backend_are_domain_errors() {
    let (service, _clock, fake) = setup(snap(7));

    let error = unnegotiated(&service, &fake).await.unwrap_err();
    assert_eq!(error.code, -32000);
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::ProtocolNotNegotiated);
    assert_eq!(error_data.recovery, HistoryRecovery::Renegotiate);
    assert_eq!(error_data.effect.execution, ExecutionEffect::NotReady);

    let error = service
        .handle(
            &open_params(1, "s1", 50, 0),
            HistoryServiceRequest::new(true, None, limits()),
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::BackendUnavailable);
}

#[tokio::test]
async fn unknown_action_and_wrong_version_are_invalid_params() {
    let (service, _clock, fake) = setup(snap(7));
    let unknown = json!({
        "version": 2,
        "action": "nope",
        "sessionId": "s1",
        "requestKey": "1"
    });
    let error = call(&service, &fake, limits(), unknown).await.unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());

    let mut wrong_version = open_params(1, "s1", 50, 0);
    wrong_version["version"] = json!(3);
    let error = call(&service, &fake, limits(), wrong_version)
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());
}

#[tokio::test]
async fn oversized_page_is_budget_exceeded_and_keeps_window() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let mut oversized = big_page(200, 3);
    oversized.history_revision = peri_acp_types::history_v2::DecimalU64::new(7);
    fake.set_page_result(Ok(oversized));
    let small = HistoryLimits {
        max_response_bytes: 100,
        ..limits()
    };
    let error = call(
        &service,
        &fake,
        small,
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::BudgetExceeded);
    assert_eq!(data(&error).recovery, HistoryRecovery::ReduceScope);
    assert_eq!(service.window_count(), 1);

    // 预算失败不关闭窗口：同一窗口在正常预算下仍可继续读取。
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect("window remains usable after budget failure");
}

#[tokio::test]
async fn close_during_page_discards_stale_page() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let gate = fake.gate_next_page();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_window = Window {
        id: window.id.clone(),
        token: window.token.clone(),
        ticket: window.ticket.clone(),
    };
    let task = tokio::spawn(async move {
        call(
            &task_service,
            &task_fake,
            limits(),
            page_params(
                2,
                "s1",
                &task_window.id,
                &task_window.token,
                &task_window.ticket,
                None,
                "before",
                50,
            ),
        )
        .await
    });
    fake.page_entered.notified().await;
    call(
        &service,
        &fake,
        limits(),
        close_params(3, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .expect("close during page");
    gate.notify_one();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowClosed);
}

#[tokio::test]
async fn seek_updates_home_cursor_for_later_pages() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    fake.set_seek_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    let value = call(
        &service,
        &fake,
        limits(),
        seek_params(2, "s1", &window.id, &window.token, &window.ticket, "e42"),
    )
    .await
    .expect("seek succeeds");
    let page = parse_page(&value);
    assert_cursor(&page.before_cursor, &window.id, "before", "b1");

    call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect("page after seek");
    let calls = fake.page_calls.lock();
    assert_eq!(calls.last().unwrap().raw_cursor, "b1");
}

#[tokio::test]
async fn page_without_home_cursor_is_invalid_cursor() {
    let mut opened = snap(7);
    opened.page.before_cursor = None;
    let (service, _clock, fake) = setup(opened);
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
}

#[tokio::test]
async fn open_entry_anchor_reaches_backend() {
    let (service, _clock, fake) = setup(snap(7));
    call(&service, &fake, limits(), open_entry_params(1, "s1", "e42"))
        .await
        .expect("entry open");
    let calls = fake.open_calls.lock();
    match &calls[0].anchor {
        HistoryAnchor::Entry(anchor) => assert_eq!(anchor.entry_id, "e42"),
        other => panic!("expected entry anchor, got {other:?}"),
    }
    assert_eq!(calls[0].before, 25);
    assert_eq!(calls[0].after, 24);
}
