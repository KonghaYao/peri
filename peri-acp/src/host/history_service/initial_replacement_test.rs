//! FIX-05：initial 窗口替换「先准备、后提交」。
//!
//! 全部使用 `FakeBackend` + `ManualClock`，覆盖后端失败、预算失败、取消/超时、
//! 准备期间 `close_session` 以及窗口配额边界；断言失败后旧窗口/旧实时流仍可用。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use peri_acp_types::history_v2::{
    HistoryBackendError, HistoryErrorReason, HistoryInitialRequest, HistoryLimits,
    HistoryLiveBoundary, HistoryV2Backend,
};

use super::backend_test::{
    big_page, body_params, limits, opaque_page, open_params, page_data, page_params, snap,
    FakeBackend, ManualClock,
};
use super::initial::HistoryInitialContext;
use super::ledger::LedgerLimits;
use super::service_test::{call, reason, setup, window_of, Window};
use super::state::HistoryClock;
use super::*;
use crate::host::live_delivery::{LiveDelivery, PublishOutcome};

fn request(key: u64) -> HistoryInitialRequest {
    HistoryInitialRequest {
        version: 2,
        request_key: DecimalU64::new(key),
        limit: 50,
        target_bytes: 524288,
    }
}

fn context(fake: &Arc<FakeBackend>, live: &Arc<LiveDelivery>) -> HistoryInitialContext {
    HistoryInitialContext {
        session_id: "s1".to_owned(),
        backend: Some(fake.clone() as Arc<dyn HistoryV2Backend>),
        negotiated: true,
        limits: limits(),
        runtime_generation: "runtime-9".to_owned(),
        execution_ready: true,
        live: Some(Arc::clone(live)),
        live_boundary: HistoryLiveBoundary {
            stream_id: String::new(),
            after_seq: DecimalU64::new(0),
        },
        reserved_response_bytes: 0,
    }
}

fn snap_with(
    page: peri_acp_types::history_v2::HistoryPageData,
) -> peri_acp_types::history_v2::HistoryOpenedSnapshot {
    let mut opened = snap(7);
    opened.page = page;
    opened.page.history_revision = DecimalU64::new(7);
    opened.page.committed_tail = Some(super::backend_test::tail_ref());
    opened.page.projection_version = 3;
    opened
}

fn publish(live: &LiveDelivery, text: &str) -> Value {
    match live.publish(
        "s1",
        json!({
            "sessionId": "s1",
            "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }
        }),
    ) {
        PublishOutcome::Send(payload) => payload,
        other => panic!("活动流必须直发：{other:?}"),
    }
}

async fn assert_window_readable(
    service: &HistoryV2Service,
    fake: &Arc<FakeBackend>,
    window: &Window,
) {
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    call(
        service,
        fake,
        limits(),
        page_params(
            99,
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
    .expect("旧窗口仍可翻页");
    call(
        service,
        fake,
        limits(),
        body_params(100, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .expect("旧窗口已签发的 bodyRef 仍可读取");
}

#[tokio::test]
async fn initial_backend_failure_keeps_old_window_and_live_identity() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    let first_window = window_of(&first.page);
    let first_stream = first.live_boundary.stream_id.clone();
    assert_eq!(
        publish(&live, "one")["_meta"]["peri.delivery"]["eventSeq"],
        "1"
    );

    fake.set_open_result(Err(HistoryBackendError::BackendUnavailable));
    let error = service
        .initial(request(2), context(&fake, &live))
        .await
        .expect_err("后端失败必须失败");
    assert_eq!(reason(&error), HistoryErrorReason::BackendUnavailable);

    assert_eq!(service.window_count(), 1, "旧窗口必须保留");
    assert_eq!(service.opening_window_count(), 0, "候选窗口必须回收");
    assert!(!live.has_candidate("s1"), "候选流必须回收");
    assert!(
        live.has_stream_with_id("s1", &first_stream),
        "旧 streamId 不得因失败的 initial 改变"
    );
    assert!(!live.is_paused("s1"), "旧流仍处于直发状态");
    assert_window_readable(&service, &fake, &first_window).await;

    let next = publish(&live, "two");
    assert_eq!(
        next["_meta"]["peri.delivery"]["streamId"], first_stream,
        "失败后实时事件仍由旧流承载"
    );
    assert_eq!(next["_meta"]["peri.delivery"]["eventSeq"], "2");
}

#[tokio::test]
async fn initial_budget_failure_keeps_old_window_and_live_stream() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    let first_window = window_of(&first.page);
    let first_stream = first.live_boundary.stream_id.clone();

    fake.set_open_result(Ok(snap_with(big_page(4096, 20))));
    let mut budgeted = context(&fake, &live);
    budgeted.limits = HistoryLimits {
        target_page_bytes: 65536,
        max_response_bytes: 65536,
        ..limits()
    };
    let error = service
        .initial(
            HistoryInitialRequest {
                version: 2,
                request_key: DecimalU64::new(2),
                limit: 50,
                target_bytes: 65536,
            },
            budgeted,
        )
        .await
        .expect_err("超预算必须失败");
    assert_eq!(reason(&error), HistoryErrorReason::BudgetExceeded);

    assert_eq!(service.window_count(), 1);
    assert_eq!(service.opening_window_count(), 0);
    assert!(!live.has_candidate("s1"));
    assert!(live.has_stream_with_id("s1", &first_stream));
    assert_window_readable(&service, &fake, &first_window).await;
}

#[tokio::test]
async fn cancelled_initial_keeps_old_window_and_live_stream() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    let first_window = window_of(&first.page);
    let first_stream = first.live_boundary.stream_id.clone();

    let _gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(2), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    assert_eq!(service.window_count(), 2, "准备期间允许一个候选槽");
    task.abort();
    assert!(task.await.is_err(), "取消必须终止准备任务");

    assert_eq!(service.window_count(), 1, "候选窗口必须回收");
    assert_eq!(service.opening_window_count(), 0);
    assert!(!live.has_candidate("s1"));
    assert!(live.has_stream_with_id("s1", &first_stream));
    assert_window_readable(&service, &fake, &first_window).await;
}

#[tokio::test]
async fn close_session_during_initial_prepare_does_not_revive_old_window() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    let first_window = window_of(&first.page);

    let _gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(2), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    // 外部关闭优先：撤销窗口与实时流，包括正在准备的候选。
    service.close_session("s1");
    live.close("s1");
    _gate.notify_one();

    let error = task
        .await
        .expect("准备任务不应 panic")
        .expect_err("会话已关闭时提交必须失败");
    assert_eq!(reason(&error), HistoryErrorReason::WindowClosed);
    assert_eq!(service.window_count(), 0);
    assert_eq!(service.opening_window_count(), 0);
    assert!(!live.has_stream("s1"), "不得复活旧实时流");
    assert!(!live.has_candidate("s1"));

    // 同一 requestKey 可重试；旧窗口不会被恢复。
    let retry = service
        .initial(request(2), context(&fake, &live))
        .await
        .expect("关闭后同一 key 可重新准备");
    assert_eq!(service.window_count(), 1);
    assert_ne!(retry.page.window_id, first_window.id);
    assert!(live.has_stream_with_id("s1", &retry.live_boundary.stream_id));
}

#[tokio::test]
async fn initial_success_switches_live_only_after_commit() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    publish(&live, "one");

    let second = service
        .initial(request(2), context(&fake, &live))
        .await
        .expect("replacement initial");
    assert_ne!(
        second.live_boundary.stream_id,
        first.live_boundary.stream_id
    );
    assert_eq!(
        second.live_boundary.after_seq.get(),
        1,
        "新流沿用旧流已发出的最后序号"
    );
    assert!(live.has_stream_with_id("s1", &second.live_boundary.stream_id));
    assert!(!live.has_stream_with_id("s1", &first.live_boundary.stream_id));
    assert_eq!(service.window_count(), 1, "提交后才关闭旧窗口");
}

#[tokio::test]
async fn initial_replacement_at_capacity_is_bounded_across_success_failure_and_replay() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    // s1 旧窗口。
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("s1 initial");
    let first_window = window_of(&first.page);
    live.resume_and_take("s1");
    // 其他会话填满剩余 7 个窗口。
    for key in 2..=8 {
        call(&service, &fake, limits(), open_params(key, "s2", 50, 0))
            .await
            .expect("fill capacity");
    }
    assert_eq!(service.window_count(), 8);

    // 替换准备期间允许 max + 1；成功后回到 max。
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(9), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    assert_eq!(service.window_count(), 9, "替换预留上限 max + 1");
    gate.notify_one();
    let replaced = task.await.expect("task").expect("replacement succeeds");
    assert_eq!(service.window_count(), 8);
    assert_eq!(service.opening_window_count(), 0);

    // 失败替换同样回到 max，且不泄漏候选/票据。
    fake.set_open_result(Err(HistoryBackendError::PermissionDenied));
    let error = service
        .initial(request(10), context(&fake, &live))
        .await
        .expect_err("failure at capacity");
    assert_eq!(reason(&error), HistoryErrorReason::PermissionDenied);
    assert_eq!(service.window_count(), 8);
    assert_eq!(service.opening_window_count(), 0);
    assert!(!live.has_candidate("s1"));

    // 同 key 重放原样返回，不再建候选或换流。
    let calls_before = fake.open_calls.lock().len();
    let replay = service
        .initial(request(9), context(&fake, &live))
        .await
        .expect("replay");
    assert_eq!(replay.delivery_id, replaced.delivery_id);
    assert_eq!(fake.open_calls.lock().len(), calls_before);
    assert_eq!(service.window_count(), 8);
    assert!(!live.has_candidate("s1"));

    // 旧窗口已被提交阶段关闭。
    fake.set_page_result(Ok(page_data(7, Some("b1"), Some("a1"))));
    let stale = call(
        &service,
        &fake,
        limits(),
        page_params(
            11,
            "s1",
            &first_window.id,
            &first_window.token,
            &first_window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect_err("旧窗口在成功替换后才关闭");
    assert_eq!(reason(&stale), HistoryErrorReason::WindowClosed);
}

/// FIX-05 顺序修正：候选流必须在 `backend.open` 之前建立。
///
/// gated open 期间产生的 live 事件进入候选缓冲；成功时 boundary 与事件序号连续，
/// 失败时候选被回收且旧 streamId / next_seq 不变。
#[tokio::test]
async fn candidate_starts_before_backend_open_and_preserves_sequence() {
    let (service, _clock, fake) = setup(snap_with(opaque_page()));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    let first_stream = first.live_boundary.stream_id.clone();
    assert_eq!(
        publish(&live, "one")["_meta"]["peri.delivery"]["eventSeq"],
        "1"
    );

    // ── 成功路径：open 期间的事件进入候选，旧流不动 ──
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(2), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    assert!(
        live.has_candidate("s1"),
        "候选流必须在 backend.open 之前创建"
    );
    assert!(matches!(
        live.publish(
            "s1",
            json!({ "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk" } })
        ),
        PublishOutcome::Buffered
    ));
    assert_eq!(live.candidate_buffered_len("s1"), 1);
    assert!(
        live.has_stream_with_id("s1", &first_stream),
        "后端读取期间旧流身份不变"
    );
    assert_eq!(live.buffered_len("s1"), 0, "旧流缓冲不得被候选事件污染");

    gate.notify_one();
    let second = task.await.expect("task").expect("replacement succeeds");
    assert_eq!(second.live_boundary.after_seq.get(), 1);
    assert_ne!(second.live_boundary.stream_id, first_stream);
    assert!(live.has_stream_with_id("s1", &second.live_boundary.stream_id));
    assert!(!live.has_stream_with_id("s1", &first_stream));
    let pending = live.resume_and_take("s1");
    assert_eq!(pending.len(), 1, "open 期间的事件必须由候选补发");
    assert_eq!(
        pending[0]["_meta"]["peri.delivery"]["streamId"],
        second.live_boundary.stream_id
    );
    assert_eq!(pending[0]["_meta"]["peri.delivery"]["eventSeq"], "2");

    // ── 失败路径：候选回收，旧流身份/序号不变 ──
    fake.set_open_result(Err(HistoryBackendError::BackendUnavailable));
    let gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(3), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    assert!(live.has_candidate("s1"));
    assert!(matches!(
        live.publish(
            "s1",
            json!({ "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk" } })
        ),
        PublishOutcome::Buffered
    ));
    gate.notify_one();
    let error = task
        .await
        .expect("task")
        .expect_err("backend failure must fail initial");
    assert_eq!(reason(&error), HistoryErrorReason::BackendUnavailable);
    assert!(!live.has_candidate("s1"), "失败必须回收候选");
    assert!(
        live.has_stream_with_id("s1", &second.live_boundary.stream_id),
        "失败后旧流身份不变"
    );
    let next = publish(&live, "three");
    assert_eq!(
        next["_meta"]["peri.delivery"]["streamId"],
        second.live_boundary.stream_id
    );
    assert_eq!(
        next["_meta"]["peri.delivery"]["eventSeq"], "3",
        "候选事件被丢弃后旧流序号仍从 afterSeq+1 继续"
    );
    assert_eq!(service.window_count(), 1);
    assert_eq!(service.opening_window_count(), 0);
}

/// §8.3 timeout 注入点：短 initial 期限 + gated backend。
///
/// 超时后旧窗口/旧 live 保留，候选窗口与候选流回收，同 requestKey 可重试。
#[tokio::test]
async fn initial_timeout_keeps_old_state_and_allows_same_key_retry() {
    let clock = Arc::new(ManualClock::new());
    let clock_ref: Arc<dyn HistoryClock> = clock.clone();
    let service = Arc::new(HistoryV2Service::with_ledger_limits_and_timeout(
        clock_ref,
        LedgerLimits::default(),
        Duration::from_millis(50),
    ));
    let fake = Arc::new(FakeBackend::new(snap_with(opaque_page())));
    let live = Arc::new(LiveDelivery::new());
    let first = service
        .initial(request(1), context(&fake, &live))
        .await
        .expect("first initial");
    live.resume_and_take("s1");
    let first_window = window_of(&first.page);
    let first_stream = first.live_boundary.stream_id.clone();

    let _gate = fake.gate_next_open();
    let task_service = service.clone();
    let task_fake = fake.clone();
    let task_live = live.clone();
    let task = tokio::spawn(async move {
        task_service
            .initial(request(2), context(&task_fake, &task_live))
            .await
    });
    fake.open_entered.notified().await;
    let error = task
        .await
        .expect("task")
        .expect_err("gated open must time out");
    assert_eq!(reason(&error), HistoryErrorReason::Timeout);

    assert_eq!(service.window_count(), 1, "超时后旧窗口保留");
    assert_eq!(service.opening_window_count(), 0, "候选窗口必须回收");
    assert!(!live.has_candidate("s1"), "候选流必须回收");
    assert!(
        live.has_stream_with_id("s1", &first_stream),
        "超时不改变旧 streamId"
    );
    assert!(!live.is_paused("s1"), "旧流仍处于直发状态");
    assert_window_readable(&service, &fake, &first_window).await;

    // 账本已标记可重试：同一 key 在无屏障时成功替换。
    let retry = service
        .initial(request(2), context(&fake, &live))
        .await
        .expect("same key retry after timeout");
    assert_eq!(service.window_count(), 1);
    assert_ne!(retry.page.window_id, first_window.id);
    assert!(live.has_stream_with_id("s1", &retry.live_boundary.stream_id));
}
