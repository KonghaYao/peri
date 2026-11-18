//! 取消、总期限与入站预算（设计 §12.2、§12.4）。
//!
//! 全部使用 `FakeBackend` + `ManualClock`；期限测试用 tokio 虚拟时钟，不真等 10 秒。

use serde_json::json;

use peri_acp_types::history_v2::{
    ExecutionEffect, HistoryEffectState, HistoryErrorReason, HistoryRecovery,
};

use super::backend_test::{limits, open_params, snap};
use super::service_test::{assert_cursor, call, data, parse_page, setup};
use super::HISTORY_MAX_PARAMS_BYTES;

#[tokio::test]
async fn cancelled_open_releases_placeholder_and_allows_immediate_retry() {
    let (service, _clock, fake) = setup(snap(7));
    // 永不释放：请求停在 backend.open；随后 abort 模拟连接取消。
    let _gate = fake.gate_next_open();
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
    task.abort();
    assert!(task.await.is_err(), "aborted task reports cancellation");

    // Drop 内清理必须已发生：没有 Opening 占位，也没有泄漏窗口。
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);

    // 同键无需等待 120s 的 InProgress 过期即可重试；第二次 open 没有 gate。
    let value = call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .expect("retry after cancellation");
    let page = parse_page(&value);
    assert_eq!(page.history_revision.get(), 7);
    assert_eq!(service.window_count(), 1);
    assert_eq!(service.opening_window_count(), 0);
    assert_cursor(&page.before_cursor, &page.window_id, "before", "b0");
}

#[tokio::test(start_paused = true)]
async fn request_timeout_releases_placeholder_and_allows_immediate_retry() {
    // 生产默认期限 10s；暂停时钟在等待中自动推进，不真等。
    let (service, _clock, fake) = setup(snap(7));
    // backend 永不返回。
    let _gate = fake.gate_next_open();
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

    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code, -32000);
    let error_data = data(&error);
    assert_eq!(error_data.reason, HistoryErrorReason::Timeout);
    assert_eq!(error_data.recovery, HistoryRecovery::Retry);
    assert_eq!(error_data.effect.execution, ExecutionEffect::Unchanged);
    assert_eq!(error_data.effect.history, HistoryEffectState::NotPublished);
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);

    // 超时 drop future 后同键立即可重试，无占位泄漏。
    let value = call(&service, &fake, limits(), open_params(1, "s1", 50, 0))
        .await
        .expect("retry after timeout");
    let page = parse_page(&value);
    assert_eq!(page.history_revision.get(), 7);
    assert_eq!(service.window_count(), 1);
    assert_eq!(service.opening_window_count(), 0);
}

#[tokio::test]
async fn oversized_params_are_invalid_before_decode_and_state_registration() {
    let (service, _clock, fake) = setup(snap(7));
    let mut params = open_params(1, "s1", 50, 0);
    params["padding"] = json!("x".repeat(HISTORY_MAX_PARAMS_BYTES));

    let error = call(&service, &fake, limits(), params).await.unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.data.is_none());
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.ledger_entry_count(), 0);
    assert!(fake.open_calls.lock().is_empty());
}
