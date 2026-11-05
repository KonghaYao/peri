//! 异常退出与再进入的提交边界（设计 §9「MQ 已移交／进程退出」一行、§10 阶段 5）。
//!
//! 夹具仍然只用临时库，但每个「进程」是**独立门面 + 独立 `SessionManager`**：它们
//! 之间只共享那个 SQLite 文件。这样断言的是「已提交内容可见、未提交内容不可见」，
//! 而不是同一连接内的内存状态。
//!
//! 真实进程边界（`peri acp` 子进程）由 `peri-tui/tests/session_recovery_process.rs`
//! 覆盖；本文件证明的是同一库文件上存储与恢复语义的正确性。

use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::recovery::{RecoverySkip, RecoverySkipReason};
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use sqlx::Connection as _;

/// 1. 已提交的 `working` + 已提交的 MQ 行 → 新进程进入时恰好一条提醒、恰好一个 turn。
///
/// 待发行取被动 Kind（`Info`）：它跨进程可见但不授予执行资格，本轮由恢复提醒驱动；
/// 提醒是**追加**一条，不顶替已有待发内容。
#[tokio::test]
async fn committed_working_and_queue_recover_once_in_a_new_host() {
    let workspace = tempfile::tempdir().unwrap();
    let mut exited = RecoveryHarness::durable(workspace.path()).await;
    exited.create_session().await;
    let thread_id = exited.thread_id.clone();
    exited.set_working(&thread_id).await;
    exited
        .queue(&thread_id)
        .enqueue(QueuedMessage::new(
            MessageKind::Info,
            MessageSource::SystemInjected,
            BaseMessage::human("继续"),
        ))
        .await
        .unwrap();
    // 退出前未收尾：不给 Stop、不写 idle、不移交 MQ。池随进程消失，只剩已提交事实。
    drop(exited);

    let mut host = RecoveryHarness::durable(workspace.path()).await;
    host.load_session(&thread_id).await;
    host.spawn_continuation_scheduler();

    let queued = host
        .queue(&thread_id)
        .query(peri_acp_types::session::MqFilter::default())
        .await
        .unwrap();
    assert_eq!(queued.len(), 1, "已提交的待发行必须在新进程可见");

    let recovery = host.recovery();
    let report = recovery.enter(&thread_id).await.expect("恢复准备必须成功");
    assert_eq!(report.recovered, vec![thread_id.clone()]);
    assert_eq!(
        host.recovery_rows(&thread_id).await.len(),
        1,
        "只允许一条恢复提醒"
    );
    assert_eq!(
        host.queue(&thread_id)
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        2,
        "提醒追加在待发行之后，不能顶替它们"
    );

    recovery.launch(&report).await;
    wait_for("恢复 turn 调用模型", || async {
        host._mock.matched()
    })
    .await;
    host._mock.assert_async().await;
}

/// 1b. 已提交的 `working` + 用户待发行：进入后的恢复提醒不得被失败水位压掉。
///
/// 场景：进入时队列里已有一条 `ensure_processing` 的用户待发输入；`enter` 注入提醒后，
/// 一次在 Receive 之前就失败的尝试把失败水位推到了提醒之后。`launch` 的 continuation
/// 仍必须真的跑 turn（模型请求），提醒与用户输入按既有 MQ 语义被处理。
#[tokio::test]
async fn recovery_reminder_survives_an_earlier_failed_attempt() {
    let workspace = tempfile::tempdir().unwrap();
    let mut exited = RecoveryHarness::durable(workspace.path()).await;
    exited.create_session().await;
    let thread_id = exited.thread_id.clone();
    exited.set_working(&thread_id).await;
    exited
        .queue(&thread_id)
        .enqueue(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human("crash me after enqueue"),
        ))
        .await
        .unwrap();
    drop(exited);

    let mut host = RecoveryHarness::durable(workspace.path()).await;
    host.load_session(&thread_id).await;
    host.spawn_continuation_scheduler();

    let recovery = host.recovery();
    let report = recovery.enter(&thread_id).await.expect("恢复准备必须成功");
    assert_eq!(report.recovered, vec![thread_id.clone()]);
    assert_eq!(
        host.recovery_rows(&thread_id).await.len(),
        1,
        "进入只注入一条提醒"
    );
    // 失败水位是「该次尝试开始时的 MQ 水位」：一个在 Receive 之前失败的尝试
    // （例如 attach 被并发预留拒绝）会把刚注入的提醒也算作已尝试。
    let watermark = host.queue(&thread_id).admission_watermark().await.unwrap();
    assert!(watermark.is_some(), "提醒必须在队列中");
    host.cfg
        .session_manager
        .get_session(&thread_id)
        .expect("会话运行时仍在")
        .activation
        .record_failed_attempt(watermark);

    recovery.launch(&report).await;

    wait_for("恢复 turn 调用模型且提醒被移交", || async {
        let model_turn = host
            .recorded
            .notifications()
            .iter()
            .any(|(method, params)| {
                method == "session/update"
                    && params["update"]["sessionUpdate"] == "agent_message_chunk"
            });
        model_turn && host.recovery_rows(&thread_id).await.is_empty()
    })
    .await;
    wait_for("用户待发输入被交付", || async {
        host.recorded
            .notifications()
            .iter()
            .any(|(method, params)| {
                method == "peri/agent_event"
                    && params["event_json"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("user_input_delivered")
            })
    })
    .await;
    assert!(
        host.recovery_rows(&thread_id).await.is_empty(),
        "失败水位不得让恢复重复注入提醒"
    );
}

/// 2. 未提交的 `working` 不可见：进入既不恢复，也不落任何提醒；提交后才恢复。
///
/// 用同库上的第二个连接持有写事务来模拟「崩溃前未提交」：只要事务没提交，
/// 恢复读取到的仍是 `idle`。回滚与提交各验一次。
#[tokio::test]
async fn uncommitted_working_is_invisible_until_the_transaction_commits() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    let recovery = harness.recovery();
    let mut writer = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();

    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET work_state = 'working' WHERE id = ?1")
        .bind(&thread_id)
        .execute(&mut writer)
        .await
        .unwrap();

    let pending = recovery.enter(&thread_id).await.expect("未提交不是错误");
    assert!(
        pending.recovered.is_empty(),
        "未提交的工作意图不得授权恢复：{pending:?}"
    );
    assert_eq!(
        pending.skipped,
        vec![RecoverySkip {
            thread_id: thread_id.clone(),
            reason: RecoverySkipReason::Idle,
        }]
    );
    assert!(
        harness.recovery_rows(&thread_id).await.is_empty(),
        "不恢复就更不得注入提醒"
    );
    assert!(!harness._mock.matched(), "未提交不得启动 turn");

    sqlx::query("ROLLBACK").execute(&mut writer).await.unwrap();
    let rolled_back = recovery.enter(&thread_id).await.unwrap();
    assert!(
        rolled_back.recovered.is_empty(),
        "回滚后仍未工作：{rolled_back:?}"
    );

    // 同一意图真正提交之后，新进入必须恢复。
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET work_state = 'working' WHERE id = ?1")
        .bind(&thread_id)
        .execute(&mut writer)
        .await
        .unwrap();
    sqlx::query("COMMIT").execute(&mut writer).await.unwrap();
    writer.close().await.unwrap();

    let committed = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(committed.recovered, vec![thread_id.clone()]);
    assert_eq!(harness.recovery_rows(&thread_id).await.len(), 1);
}

/// 3. 同一根跨「进程」重复进入：恢复 turn 只发生一次，提醒不会二次注入。
///
/// 第一次进入完成一个 turn；随后新进程再次 load 同一个 thread，必须零注入、零 turn。
#[tokio::test]
async fn reentry_in_a_third_host_does_not_repeat_the_recovered_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let mut exited = RecoveryHarness::durable(workspace.path()).await;
    exited.create_session().await;
    let thread_id = exited.thread_id.clone();
    exited.set_working(&thread_id).await;
    drop(exited);

    let mut first = RecoveryHarness::durable(workspace.path()).await;
    first.load_session(&thread_id).await;
    first.spawn_continuation_scheduler();
    let recovery = first.recovery();
    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);
    recovery.launch(&report).await;
    wait_for("恢复 turn 调用模型", || async {
        first._mock.matched()
    })
    .await;
    first._mock.assert_async().await;
    wait_for("提醒被 Receive 移交", || async {
        first.recovery_rows(&thread_id).await.is_empty()
    })
    .await;
    drop(first);

    let mut again = RecoveryHarness::durable(workspace.path()).await;
    again.load_session(&thread_id).await;
    let second = again.recovery().enter(&thread_id).await.unwrap();
    assert!(
        second.recovered.is_empty(),
        "重复进入不得再注入提醒：{second:?}"
    );
    assert!(again.recovery_rows(&thread_id).await.is_empty());
    // 没有 continuation 消费者时也就没有 turn；给调度留出窗口再确认模型未被调用。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!again._mock.matched(), "重复进入不得启动第二个 turn");
}
