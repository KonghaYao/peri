//! 在途请求被丢弃之后的同实例恢复（本地可控故障，不联网；夹具见
//! `recovery_fixture_test.rs`）。
//!
//! Fable 反例：`turso_serverless` 0.1.3 上放弃一个**已经发出**的在途请求（取消或预算超时）
//! 之后，同一条连接上的后续请求一律在传输层失败（分类后是 `Unavailable`），必须重开连接
//! 才能继续。本文件用可控假传输在本地确定复现这条故障（命中档位的调用永远挂起，调用方只能
//! 丢弃它；被丢弃的那条连接此后一律拒绝请求），断言 adapter 的反应：
//!
//! 1. **取消读**：同实例的下一次读取重建连接并给出正确答案（不在 → `NotFound`；在 → 那个
//!    会话真实的绑定分类），而不是 `Unavailable`；
//! 2. **取消写**：同一条连接上已经发出的效果不因取消而改变（从未提交的仍是零、已提交的仍是
//!    一份）；重建的只是连接，**绝不自动重发**未知 mutation——重发与否是调用方的事；
//! 3. **旧代际的失效不碰新连接**：迟到任务拿旧代际号标记失效时，新连接照常服务，且不会再建；
//!    反过来，已经落下的失效事实也不会被更早的代际号撤销（否则一条无法证明的连接会被当回
//!    可用的，下一次访问不再重建、直接在死连接上失败）；失败的替换尝试也不波及装槽的那一代；
//! 4. **取守卫前复核**：重建判定与取守卫之间落下的失效事实会让本次调用如实失败（什么都不发），
//!    下一次访问才重建——不把请求发到一条已经不可证明的连接上；
//! 5. **只读重连只读**：重建不写 schema、不写账本。

use std::sync::atomic::Ordering;

use peri_acp_types::session_resources::{BindingState, SessionResourceErrorKind};
use peri_acp_types::thread::ThreadId;

use super::mutation::StoreAccess;
use super::recovery_fixture_tests::{
    abandon_read, abandon_write, ledger_sql, Hang, Harness, ABANDON_AFTER,
};
use super::schema;
use crate::sessions::data::SessionDataPort;

// ─── 1. 取消读 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_abandoned_read_is_answered_correctly_on_the_same_instance() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();

    // 远端没有这个会话：正确答案是 NotFound，不是「连接不可用」。
    harness.backend.session_row.store(false, Ordering::SeqCst);
    abandon_read(&harness, &id).await;
    assert_eq!(
        harness.backend.connections(),
        1,
        "no connection yet rebuilt"
    );
    let error = harness
        .adapter
        .load_binding(&id)
        .await
        .expect_err("the session does not exist remotely");
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::NotFound),
        "expected NotFound, got {:?}",
        error.kind()
    );
    assert_eq!(
        harness.backend.connections(),
        2,
        "the abandoned generation is replaced by exactly one new connection"
    );

    // 远端有这个会话：正确答案是它真实的分类（没有绑定行的既有会话）。
    harness.backend.session_row.store(true, Ordering::SeqCst);
    abandon_read(&harness, &id).await;
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("a recovered read reaches the remote store");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(harness.backend.connections(), 3);
}

// ─── 2. 取消写 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_abandoned_write_that_never_committed_is_not_re_sent() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);

    // 批到达远端但没有提交（结果未知）：被丢弃的是**写**。后续访问只重建那条连接，
    // 没有任何一层会替调用方重发这条无法证明终态的写。
    abandon_write(&harness, &id, false).await;

    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the next access rebuilds the connection and answers");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.executed_effect_batches(),
        0,
        "the abandoned batch never applied"
    );

    let sql = ledger_sql();
    let log = harness.backend.issued();
    let qualification = log
        .iter()
        .filter(|(_, sql_text, _)| *sql_text == sql.qualify)
        .collect::<Vec<_>>();
    assert_eq!(
        qualification.len(),
        1,
        "the abandoned attempt is the only qualification write; nothing was re-sent"
    );
    assert_eq!(harness.backend.connections(), 2);
}

#[tokio::test]
async fn an_abandoned_write_that_committed_is_not_re_sent() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);

    // 批真的提交了、响应丢失：后续访问重建连接，但那份已经落下的效果**不再发一次**。
    abandon_write(&harness, &id, true).await;

    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the next access rebuilds the connection and answers");
    assert!(matches!(state, BindingState::Missing));

    let sql = ledger_sql();
    let log = harness.backend.issued();
    let qualification = log
        .iter()
        .filter(|(_, sql_text, _)| *sql_text == sql.qualify)
        .count();
    assert_eq!(qualification, 1, "the committed batch is not re-sent");
    assert_eq!(
        harness.backend.executed_effect_batches(),
        1,
        "the effect count stays exactly one"
    );
    assert_eq!(harness.backend.connections(), 2);
}

// ─── 3. 旧代际的失效不碰新连接 ────────────────────────────────────────────────

#[tokio::test]
async fn a_late_invalidation_leaves_the_new_connection_alone() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);
    abandon_read(&harness, &id).await;
    harness
        .adapter
        .load_binding(&id)
        .await
        .expect("a recovered read");
    assert_eq!(harness.backend.connections(), 2);

    // 迟到任务拿着第一代的守卫落下失效事实：它只作用于那一代。
    let late = harness.gate.lease(1);
    assert_eq!(late.generation(), 1);
    drop(late);

    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the new connection still serves");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.connections(),
        2,
        "the late invalidation did not retire the new connection"
    );
    let log = harness.backend.issued();
    assert_eq!(log.last().map(|(connection, _, _)| *connection), Some(2));
}

/// 反例：第 1 代失效 → 建第 2 代 → 第 2 代也失效 → 第 1 代的**迟到**失效。
///
/// 失效事实如果按「相等」记（后写覆盖），迟到的那次会把记录写回第 1 代，已经无法证明的
/// 第 2 代就被当成可用的：下一次访问不再重建，直接在死连接上失败。
#[tokio::test]
async fn a_stale_invalidation_does_not_revive_an_invalidated_connection() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);

    // 第 1 代被放弃（失效）→ 下一次访问重建出第 2 代并正常服务。
    abandon_read(&harness, &id).await;
    harness
        .adapter
        .load_binding(&id)
        .await
        .expect("a recovered read");
    assert_eq!(harness.backend.connections(), 2);

    // 第 2 代也被放弃：这一代的失效事实落下。
    abandon_read(&harness, &id).await;

    // 迟到的第 1 代失效（同一份旧守卫在更晚的时刻被丢弃）不能撤销第 2 代的失效事实。
    let late = harness.gate.lease(1);
    assert_eq!(late.generation(), 1);
    drop(late);

    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the invalidated generation is replaced, not reused");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.connections(),
        3,
        "the invalidated generation is rebuilt once more instead of being reused"
    );
    let log = harness.backend.issued();
    assert_eq!(
        log.last().map(|(connection, _, _)| *connection),
        Some(3),
        "the read is served by the rebuilt connection"
    );
}

/// 失败的替换尝试（重核实的读取在途被丢弃）不波及后来真正装进槽位的那一代。
///
/// 第 2 代从未服务过任何业务请求就退场了；它的失效事实若把水位抬到第 3 代之上，
/// 一条健康的连接会被反复误判成失效。
#[tokio::test]
async fn a_failed_replacement_does_not_retire_the_connection_that_took_over() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);

    // 第 1 代失效；随后一次重建在**重核实**阶段被丢弃：替换连接没有装回槽位。
    abandon_read(&harness, &id).await;
    harness.backend.hang_next(Hang::Read);
    let abandoned = tokio::time::timeout(ABANDON_AFTER, harness.adapter.load_binding(&id)).await;
    assert!(
        abandoned.is_err(),
        "the rebuilding attempt was expected to be abandoned in flight"
    );
    assert_eq!(
        harness.backend.connections(),
        2,
        "the replacement was built but never installed"
    );

    // 下一次访问按第 1 代的失效事实重建：第 3 代装进槽位并服务读取。
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("a recovered read");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(harness.backend.connections(), 3);
    let log = harness.backend.issued();
    assert_eq!(log.last().map(|(connection, _, _)| *connection), Some(3));

    // 迟到的第 2 代失效（失败尝试那一代）不能把服务中的第 3 代判成失效：没有多建连接。
    let stale = harness.gate.lease(2);
    drop(stale);
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the serving connection stays usable");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.connections(),
        3,
        "a stale attempt's invalidation does not retire the serving connection"
    );
    let log = harness.backend.issued();
    assert_eq!(log.last().map(|(connection, _, _)| *connection), Some(3));
}

/// 并发重建：多个调用者同时看到同一条失效代际，只有一个替换连接进入槽位。
///
/// 中途被放弃的那一次（它的重核实读取挂在传输里）在**它自己那一代**上落下失效事实；
/// 装进槽位的那一代号更大，照常服务，且不会被多重建一次。
#[tokio::test]
async fn concurrent_rebuilds_install_exactly_one_connection_and_keep_it_healthy() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);
    abandon_read(&harness, &id).await;

    // 挂起下一次读取，让两个重建尝试真正交叠：先到者挂在重核实里（它的替换连接不会装槽），
    // 后到者建立自己的连接并装进槽位。
    harness.backend.hang_next(Hang::Read);
    let (abandoned, served) = tokio::join!(
        tokio::time::timeout(ABANDON_AFTER, harness.adapter.load_binding(&id)),
        harness.adapter.load_binding(&id),
    );
    assert!(
        abandoned.is_err(),
        "the rebuilding attempt that hit the hang was expected to be abandoned"
    );
    let state = served.expect("the concurrent attempt serves the read");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.connections(),
        3,
        "one replacement was installed; the other one was abandoned mid-verify"
    );

    // 装进槽位的那一代仍然健康：被放弃的那次尝试的失效事实只作用于它自己那一代。
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the installed connection keeps serving");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(
        harness.backend.connections(),
        3,
        "no extra rebuild: the abandoned attempt does not retire the installed connection"
    );
    let log = harness.backend.issued();
    assert_eq!(log.last().map(|(connection, _, _)| *connection), Some(3));
}

#[tokio::test]
async fn a_read_only_reconnect_writes_nothing() {
    let harness = Harness::open(StoreAccess::ReadOnly).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);
    abandon_read(&harness, &id).await;
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("a recovered read");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(harness.backend.connections(), 2);

    let sql = ledger_sql();
    let log = harness.backend.issued();
    assert!(
        log.iter().all(|(_, sql_text, _)| *sql_text != sql.qualify
            && *sql_text != sql.closure
            && *sql_text != sql.resolve
            && !sql_text.starts_with("CREATE")),
        "a read-only reconnect issues no DDL, no schema write and no ledger write: {:?}",
        log.iter()
            .map(|(_, sql_text, _)| *sql_text)
            .collect::<Vec<_>>()
    );
    assert_eq!(harness.backend.executed_effect_batches(), 0);
}

/// 重建判定与取守卫之间落下的失效事实：本次调用什么也不发，下一次访问才重建。
///
/// 反例：判定（`is_invalid`）与取读锁之间没有原子性，另一条在途调用在同一刻被放弃会把**当前**
/// 这一代记为失效。若守卫照旧交出去，调用方就会在一条已经不可证明的连接上发请求。
#[tokio::test]
async fn a_generation_retired_before_the_guard_is_taken_fails_without_sending() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    harness.backend.session_row.store(true, Ordering::SeqCst);

    // 第 1 代被放弃（失效）→ 下一次访问会重建。让重建连接的**重核实读取**顺手把新铸的那一代
    // 记为失效：这就是那个空窗（另一条在途调用在同一刻被放弃，落下同一个事实）。
    abandon_read(&harness, &id).await;
    harness.backend.retire_generation_on_next_read(2);

    let refused = harness
        .adapter
        .load_binding(&id)
        .await
        .expect_err("a connection retired before the guard is taken is never used");
    assert!(
        matches!(refused.kind(), SessionResourceErrorKind::Unavailable { .. }),
        "expected a conservative Unavailable, got {:?}",
        refused.kind()
    );
    // 那条连接只服务过只读身份重核实，没有收到任何业务请求（本调用什么都没发）。
    let identity_sql: Vec<&'static str> = schema::identity_read_plan()
        .iter()
        .map(|spec| spec.sql)
        .collect();
    let issued = harness.backend.issued();
    assert!(
        issued
            .iter()
            .filter(|(connection, _, _)| *connection == 2)
            .all(|(_, sql, _)| identity_sql.contains(sql)),
        "the retired connection never served a business request"
    );
    assert_eq!(
        harness.backend.connections(),
        2,
        "no automatic retry: rebuilding is the next access's job"
    );

    // 下一次访问重建（第 3 条）并正常服务。
    let state = harness
        .adapter
        .load_binding(&id)
        .await
        .expect("the next read rebuilds and serves");
    assert!(matches!(state, BindingState::Missing));
    assert_eq!(harness.backend.connections(), 3);
    let issued = harness.backend.issued();
    assert_eq!(issued.last().map(|(connection, _, _)| *connection), Some(3));
}
