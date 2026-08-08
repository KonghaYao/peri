//! 聚合器 P0 契约测试（§12 测试前提：纯函数测试，内存 Y.Doc）。
//!
//! 覆盖：幂等重放（§4.8 向量 3）、user_message 幂等（§6.5）、终态守卫
//! （§4.8 向量 4）、interrupted 校准恰一次（§6.3 例外）、gap 计数（§8.5）、
//! session 终态（§8.2）、双 Doc 事务顺序（§7.4）。

use serde_json::json;

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::schema::{
    ActiveTurnProjection, BlockVisibility, PermissionOptions, SessionStatus, TurnStatus,
};

use yrs::{GetString, Map, Transact, WriteTxn};

use crate::state::aggregator::{Aggregator, ApplyReason};
use crate::state::chat_writer;
use crate::state::doc_pair::DocPair;
use crate::state::factory::{Factory, ROOT};
use crate::state::normalized::{EventBody, NormalizedEvent};
use crate::state::view_store::{ViewStore, YrsViewStore};

fn pair() -> DocPair {
    Factory::new().create_chat_doc()
}

fn ev(session: &str, seq: u64, body: EventBody) -> NormalizedEvent {
    NormalizedEvent {
        session_id: session.to_string(),
        seq,
        epoch: 0,
        body,
    }
}

fn msg_delta(turn: &str, entry: &str, block: &str, text: &str) -> EventBody {
    EventBody::MessageDelta {
        turn_id: turn.to_string(),
        entry_id: entry.to_string(),
        block_id: block.to_string(),
        text: text.to_string(),
    }
}

fn user_msg(turn: &str, entry: &str, text: &str) -> EventBody {
    EventBody::UserMessage {
        turn_id: turn.to_string(),
        entry_id: entry.to_string(),
        text: text.to_string(),
        author_user_id: None,
        created_at: "2026-08-07T00:00:00Z".to_string(),
    }
}

fn tool_started(turn: &str, id: &str) -> EventBody {
    EventBody::ToolCallStarted {
        turn_id: turn.to_string(),
        tool_call_id: id.to_string(),
        name: "shell".to_string(),
        arguments: Some(json!({"cmd": "ls"})),
        created_at: "2026-08-07T00:00:00Z".to_string(),
    }
}

fn turn_terminal(turn: &str, status: TurnStatus) -> EventBody {
    EventBody::TurnTerminal {
        turn_id: turn.to_string(),
        status,
        completed_at: "2026-08-07T00:00:01Z".to_string(),
        public_error: None,
    }
}

fn permission_requested(id: &str, turn: &str) -> EventBody {
    EventBody::PermissionRequested {
        permission_id: id.to_string(),
        turn_id: turn.to_string(),
        tool_call_id: None,
        title: "允许执行".to_string(),
        description: None,
        options: vec![PermissionOptions::AllowOnce],
        expires_at: "2026-08-07T00:05:00Z".to_string(),
    }
}

/// 统计 chat doc 中的 entry 数。
fn entry_count(pair: &DocPair) -> usize {
    let txn = pair.chat.transact();
    chat_writer::root_map_read(&txn)
        .and_then(|root| root.get(&txn, "entries"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|m| m.len(&txn) as usize)
        .unwrap_or(0)
}

fn tool_call_count(pair: &DocPair) -> usize {
    let txn = pair.chat.transact();
    chat_writer::root_map_read(&txn)
        .and_then(|root| root.get(&txn, "tool_calls"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|m| m.len(&txn) as usize)
        .unwrap_or(0)
}

fn permission_count(pair: &DocPair) -> usize {
    let txn = pair.session.transact();
    chat_writer::root_map_read(&txn)
        .and_then(|root| root.get(&txn, "pending_permissions"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|m| m.len(&txn) as usize)
        .unwrap_or(0)
}

fn active_turn_status(pair: &DocPair) -> Option<TurnStatus> {
    let txn = pair.session.transact();
    chat_writer::root_map_read(&txn)?
        .get(&txn, "active_turn")?
        .cast::<yrs::MapRef>()
        .ok()
        .map(|m| {
            m.get(&txn, "turn_status")
                .and_then(|s| s.cast::<String>().ok())
                .map(|s| match s.as_str() {
                    "accepting" => TurnStatus::Accepting,
                    "running" => TurnStatus::Running,
                    "awaitingPermission" => TurnStatus::AwaitingPermission,
                    "cancelling" => TurnStatus::Cancelling,
                    "completed" => TurnStatus::Completed,
                    "cancelled" => TurnStatus::Cancelled,
                    "interrupted" => TurnStatus::Interrupted,
                    "failed" => TurnStatus::Failed,
                    _ => TurnStatus::Accepting,
                })
                .unwrap_or(TurnStatus::Accepting)
        })
}

// ---------------------------------------------------------------------------
// 1. 幂等重放（§4.8 向量 3）
// ---------------------------------------------------------------------------

#[test]
fn replay_same_event_is_seq_out_of_order_and_no_dup() {
    let mut p = pair();
    let mut agg = Aggregator;
    let e = ev("s1", 1, user_msg("t1", "t1:user", "hello"));
    assert!(agg.apply(&mut p, &e).applied);
    // 同 seq 重放：水位拒绝（§9.2 步骤 2），不重复创建。
    let r = agg.apply(&mut p, &e);
    assert!(!r.applied);
    assert_eq!(r.reason, Some(ApplyReason::SeqOutOfOrder));
    assert_eq!(entry_count(&p), 1);
}

#[test]
fn replay_same_business_key_new_seq_is_duplicate_idempotent() {
    let mut p = pair();
    let mut agg = Aggregator;
    // 同一 user turn 以不同 seq 重放（ACP 侧重发 user_message_chunk）。
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hello"))).applied);
    let r = agg.apply(&mut p, &ev("s1", 2, user_msg("t1", "t1:user", "hello")));
    assert_eq!(r.reason, Some(ApplyReason::DuplicateIdempotent));
    assert_eq!(entry_count(&p), 1);

    // tool_call started 重放（不同 seq）：幂等键拒绝，不重复创建。
    assert!(agg
        .apply(&mut p, &ev("s1", 3, tool_started("t1", "tc1")))
        .applied);
    let r = agg.apply(&mut p, &ev("s1", 4, tool_started("t1", "tc1")));
    assert_eq!(r.reason, Some(ApplyReason::DuplicateIdempotent));
    assert_eq!(tool_call_count(&p), 1);

    // permission requested 重放（不同 seq）：幂等键拒绝。
    assert!(agg
        .apply(&mut p, &ev("s1", 5, permission_requested("p1", "t1")))
        .applied);
    let r = agg.apply(&mut p, &ev("s1", 6, permission_requested("p1", "t1")));
    assert_eq!(r.reason, Some(ApplyReason::DuplicateIdempotent));
    assert_eq!(permission_count(&p), 1);
}

// ---------------------------------------------------------------------------
// 2. user_message 幂等（§6.5）
// ---------------------------------------------------------------------------

#[test]
fn user_message_turn_id_idempotent() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    let r = agg.apply(&mut p, &ev("s1", 2, user_msg("t1", "t1:user", "a")));
    assert_eq!(r.reason, Some(ApplyReason::DuplicateIdempotent));
    assert_eq!(entry_count(&p), 1);
}

// ---------------------------------------------------------------------------
// 3. 终态守卫：cancelled 后晚到增量丢弃（§4.8 向量 4）
// ---------------------------------------------------------------------------

#[test]
fn terminal_turn_drops_late_deltas() {
    let mut p = pair();
    let mut agg = Aggregator;
    // turn 注册 + 内容 + 终态。
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    assert!(agg
        .apply(&mut p, &ev("s1", 2, msg_delta("t1", "t1:assistant", "b1", "out")))
        .applied);
    let r = agg.apply(&mut p, &ev("s1", 3, turn_terminal("t1", TurnStatus::Cancelled)));
    assert!(r.applied);
    assert_eq!(active_turn_status(&p), Some(TurnStatus::Cancelled));

    // 晚到 delta → TurnTerminalGuard，doc 不变。
    let before = entry_count(&p);
    for seq in 4..6 {
        let r = agg.apply(
            &mut p,
            &ev("s1", seq, msg_delta("t1", "t1:assistant", "b1", "late")),
        );
        assert_eq!(r.reason, Some(ApplyReason::TurnTerminalGuard));
        assert_eq!(entry_count(&p), before);
    }
    // 晚到 tool_call updated → TurnTerminalGuard。
    let r = agg.apply(
        &mut p,
        &ev("s1", 6, EventBody::ToolCallUpdated {
            turn_id: "t1".into(),
            tool_call_id: "tc1".into(),
            arguments: Some(json!({"x": 1})),
        }),
    );
    assert_eq!(r.reason, Some(ApplyReason::TurnTerminalGuard));
}

#[test]
fn cancelling_turn_drops_late_deltas() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    // 手动置 cancelling（命令路径通常如此；此处直接写 active_turn）。
    {
        let mut txn = p.session_txn();
        let root = txn.get_or_insert_map(ROOT);
        chat_writer::set_active_turn(
            &mut txn,
            &root,
            Some(&ActiveTurnProjection {
                turn_id: "t1".into(),
                turn_status: TurnStatus::Cancelling,
                updated_at: "2026-08-07T00:00:01Z".into(),
            }),
        );
    }
    let r = agg.apply(&mut p, &ev("s1", 2, msg_delta("t1", "t1:assistant", "b1", "x")));
    assert_eq!(r.reason, Some(ApplyReason::TurnTerminalGuard));
    // cancelling → 终态事件应用（状态机迁移，§7.2）。
    let r = agg.apply(&mut p, &ev("s1", 3, turn_terminal("t1", TurnStatus::Cancelled)));
    assert!(r.applied);
    assert_eq!(active_turn_status(&p), Some(TurnStatus::Cancelled));
}

// ---------------------------------------------------------------------------
// 4. interrupted 校准恰一次（§6.3 例外 / §9.3 双条件）
// ---------------------------------------------------------------------------

#[test]
fn interrupted_calibration_exactly_once() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    // 断链：turn 置 interrupted（命令路径语义，聚合器事件亦支持）。
    assert!(agg
        .apply(&mut p, &ev("s1", 2, turn_terminal("t1", TurnStatus::Interrupted)))
        .applied);
    assert_eq!(active_turn_status(&p), Some(TurnStatus::Interrupted));

    // interrupted 状态下：非终态事件 → InterruptedGuard。
    let r = agg.apply(&mut p, &ev("s1", 3, msg_delta("t1", "t1:assistant", "b1", "x")));
    assert_eq!(r.reason, Some(ApplyReason::InterruptedGuard));

    // 带重放序依据（seq 单调）的终态事件 → 恰一次校准。
    let r = agg.apply(&mut p, &ev("s1", 4, turn_terminal("t1", TurnStatus::Completed)));
    assert!(r.applied);
    assert_eq!(active_turn_status(&p), Some(TurnStatus::Completed));

    // 校准后：任何同 turn 终态事件（高序）→ CalibrationDone。
    let r = agg.apply(&mut p, &ev("s1", 5, turn_terminal("t1", TurnStatus::Completed)));
    assert_eq!(r.reason, Some(ApplyReason::CalibrationDone));
    let r = agg.apply(&mut p, &ev("s1", 6, turn_terminal("t1", TurnStatus::Failed)));
    assert_eq!(r.reason, Some(ApplyReason::CalibrationDone));

    // 校准后 delta → TurnTerminalGuard（状态位已非 interrupted）。
    let r = agg.apply(&mut p, &ev("s1", 7, msg_delta("t1", "t1:assistant", "b1", "x")));
    assert_eq!(r.reason, Some(ApplyReason::TurnTerminalGuard));
}

#[test]
fn interrupted_low_seq_terminal_rejected() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    assert!(agg
        .apply(&mut p, &ev("s1", 5, turn_terminal("t1", TurnStatus::Interrupted)))
        .applied);
    // 乱序补推（seq 回退）→ SeqOutOfOrder（步骤 2 水位拒绝，§9.2）。
    let r = agg.apply(&mut p, &ev("s1", 3, turn_terminal("t1", TurnStatus::Completed)));
    assert_eq!(r.reason, Some(ApplyReason::SeqOutOfOrder));
    // active_turn 仍为 interrupted（未被迁移）。
    assert_eq!(active_turn_status(&p), Some(TurnStatus::Interrupted));
}

// ---------------------------------------------------------------------------
// 5. gap 计数（§8.5 / §9.4）
// ---------------------------------------------------------------------------

#[test]
fn gap_count_increments_on_seq_jump_and_clears_on_catchup() {
    let mut p = pair();
    let mut agg = Aggregator;
    // seq 连续：无 gap。
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    assert!(agg.apply(&mut p, &ev("s1", 2, user_msg("t2", "t2:user", "b"))).applied);
    assert_eq!(p.stream.gap_count, 0);
    assert!(!p.stream.gap_dirty);
    // seq 跳变：gap_count += 跳变。
    assert!(agg.apply(&mut p, &ev("s1", 5, user_msg("t3", "t3:user", "c"))).applied);
    assert_eq!(p.stream.gap_count, 2); // 期望 3，到达 5 → +2
    assert!(p.stream.gap_dirty);
    assert_eq!(p.stream.last_seq, 5);
    // 连续追平：清零 + 上报标记。
    assert!(agg.apply(&mut p, &ev("s1", 6, user_msg("t4", "t4:user", "d"))).applied);
    assert_eq!(p.stream.gap_count, 0);
    assert!(p.stream.gap_dirty);
}

#[test]
fn epoch_mismatch_marks_uncalibratable_and_rejects() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    // 新纪元帧：EpochMismatch + uncalibratable。
    let e = NormalizedEvent {
        session_id: "s1".into(),
        seq: 2,
        epoch: 1,
        body: user_msg("t2", "t2:user", "b"),
    };
    let r = agg.apply(&mut p, &e);
    assert_eq!(r.reason, Some(ApplyReason::EpochMismatch));
    assert!(p.stream.uncalibratable);
    assert!(p.stream.gap_dirty);
    // 同新纪元后续事件 → UncalibratableGap（拒绝一切投影）。
    let r = agg.apply(
        &mut p,
        &NormalizedEvent {
            session_id: "s1".into(),
            seq: 3,
            epoch: 1,
            body: user_msg("t3", "t3:user", "c"),
        },
    );
    assert_eq!(r.reason, Some(ApplyReason::UncalibratableGap));
    assert_eq!(entry_count(&p), 1);
}

#[test]
fn seq_out_of_order_rejected() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 3, user_msg("t1", "t1:user", "a"))).applied);
    let r = agg.apply(&mut p, &ev("s1", 2, user_msg("t2", "t2:user", "b")));
    assert_eq!(r.reason, Some(ApplyReason::SeqOutOfOrder));
}

// ---------------------------------------------------------------------------
// 6. session 终态（§8.2）：ended/closed/crashed 拒绝新事件
// ---------------------------------------------------------------------------

#[test]
fn closed_session_rejects_new_events() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    // SessionInfo 置 closed。
    assert!(agg
        .apply(
            &mut p,
            &ev("s1", 2, EventBody::SessionInfo {
                title: None,
                status: Some(SessionStatus::Closed),
                active_turn_id: None,
            })
        )
        .applied);
    let r = agg.apply(&mut p, &ev("s1", 3, user_msg("t2", "t2:user", "b")));
    assert_eq!(r.reason, Some(ApplyReason::SessionClosed));
}

// ---------------------------------------------------------------------------
// 7. 关联检查（§9.2 步骤 6）
// ---------------------------------------------------------------------------

#[test]
fn unknown_tool_call_and_permission_rejected() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    // tool_call updated 引用未知 tool_call_id → UnknownToolCall。
    let r = agg.apply(
        &mut p,
        &ev("s1", 2, EventBody::ToolCallUpdated {
            turn_id: "t1".into(),
            tool_call_id: "nope".into(),
            arguments: None,
        }),
    );
    assert_eq!(r.reason, Some(ApplyReason::UnknownToolCall));
    // permission resolved 引用未知 permission_id → UnknownPermission。
    let r = agg.apply(
        &mut p,
        &ev("s1", 3, EventBody::PermissionResolved {
            permission_id: "nope".into(),
            decision: PermissionDecision::Allow,
        }),
    );
    assert_eq!(r.reason, Some(ApplyReason::UnknownPermission));
}

#[test]
fn unknown_turn_rejected_for_delta() {
    let mut p = pair();
    let mut agg = Aggregator;
    // 无 active_turn 时 delta 到达：turn 未知（§9.2 步骤 6）。
    let r = agg.apply(&mut p, &ev("s1", 1, msg_delta("t1", "t1:assistant", "b1", "x")));
    assert_eq!(r.reason, Some(ApplyReason::UnknownTurn));
}

// ---------------------------------------------------------------------------
// 8. 工具结果截断（§9.5）
// ---------------------------------------------------------------------------

#[test]
fn oversized_tool_result_truncated() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    assert!(agg
        .apply(&mut p, &ev("s1", 2, tool_started("t1", "tc1")))
        .applied);
    let big = json!({"data": "x".repeat(5000)});
    let r = agg.apply(
        &mut p,
        &ev("s1", 3, EventBody::ToolCallCompleted {
            turn_id: "t1".into(),
            tool_call_id: "tc1".into(),
            result: Some(big),
            public_error: None,
        }),
    );
    assert!(r.applied);
    // result 超阈值 → 不写（None）。
    let txn = p.chat.transact();
    let root = chat_writer::root_map_read(&txn).unwrap();
    let calls = root.get(&txn, "tool_calls").unwrap().cast::<yrs::MapRef>().unwrap();
    let cm = calls.get(&txn, "tc1").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(cm.get(&txn, "result"), Some(yrs::Out::Any(yrs::Any::Null)));
    let _ = root;
}

// ---------------------------------------------------------------------------
// 9. 双 Doc 事务顺序（§7.4）：chat 先于 session
// ---------------------------------------------------------------------------

#[test]
fn chat_transaction_precedes_session() {
    let mut p = pair();
    // 注册观察：记录 update 提交顺序（经观察回调，与 DocManager 同路径）。
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let doc_chat = p.chat.clone();
    let doc_session = p.session.clone();
    let tx_chat = tx.clone();
    let tx_session = tx;
    let _sub1 = doc_chat
        .observe_update_v1(move |_, e| {
            let _ = tx_chat.send(format!("chat:{}", e.update.len()));
        })
        .unwrap();
    let _sub2 = doc_session
        .observe_update_v1(move |_, e| {
            let _ = tx_session.send(format!("session:{}", e.update.len()));
        })
        .unwrap();

    let mut agg = Aggregator;
    // user_message 同时写 chat（entry）+ session（active_turn）。
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    // 顺序断言：chat update 先于 session update。
    let mut seqs = Vec::new();
    while let Ok(s) = rx.try_recv() {
        seqs.push(s);
    }
    let chat_idx = seqs.iter().position(|s| s.starts_with("chat:")).unwrap();
    let session_idx = seqs.iter().position(|s| s.starts_with("session:")).unwrap();
    assert!(
        chat_idx < session_idx,
        "chat 事务必须先于 session 事务，got {seqs:?}"
    );
}

// ---------------------------------------------------------------------------
// 10. 微批次：apply_batch 合并为一次 chat 事务
// ---------------------------------------------------------------------------

#[test]
fn batch_merges_deltas_into_single_transaction() {
    let mut p = pair();
    // 先注册 turn（active_turn 存在，避免 UnknownTurn）。
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    // 观察回调计数（每次事务提交 +1）。
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let doc = p.chat.clone();
    let _sub = doc
        .observe_update_v1(move |_, _| {
            let _ = tx.send(());
        })
        .unwrap();
    let evs: Vec<NormalizedEvent> = (2..6)
        .map(|seq| ev("s1", seq, msg_delta("t1", "t1:assistant", "b1", "x")))
        .collect();
    let results = agg.apply_batch(&mut p, &evs);
    assert!(results.iter().all(|r| r.applied));
    // 批次 = 一次 chat 事务 → 恰好 1 个 update。
    let mut updates = 0;
    while rx.try_recv().is_ok() {
        updates += 1;
    }
    assert_eq!(updates, 1, "批次必须合并为一次事务");
    // 文本已追加。
    let txn = p.chat.transact();
    let root = chat_writer::root_map_read(&txn).unwrap();
    let entries = root.get(&txn, "entries").unwrap().cast::<yrs::MapRef>().unwrap();
    let em = entries.get(&txn, "t1:assistant").unwrap().cast::<yrs::MapRef>().unwrap();
    let blocks = em.get(&txn, "blocks").unwrap().cast::<yrs::MapRef>().unwrap();
    let bm = blocks.get(&txn, "b1").unwrap().cast::<yrs::MapRef>().unwrap();
    let text = bm.get(&txn, "text").unwrap().cast::<yrs::TextRef>().unwrap();
    assert_eq!(text.get_string(&txn), "xxxx");
    let _ = root;
}

// ---------------------------------------------------------------------------
// 11. 视图读取：reasoning 可见性
// ---------------------------------------------------------------------------

#[test]
fn reasoning_visibility_written() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);
    assert!(agg
        .apply(
            &mut p,
            &ev("s1", 2, EventBody::ReasoningDelta {
                turn_id: "t1".into(),
                entry_id: "t1:assistant".into(),
                block_id: "r1".into(),
                text: "think".into(),
                visibility: BlockVisibility::Hidden,
            })
        )
        .applied);
    let txn = p.chat.transact();
    let root = chat_writer::root_map_read(&txn).unwrap();
    let entries = root.get(&txn, "entries").unwrap().cast::<yrs::MapRef>().unwrap();
    let em = entries.get(&txn, "t1:assistant").unwrap().cast::<yrs::MapRef>().unwrap();
    let blocks = em.get(&txn, "blocks").unwrap().cast::<yrs::MapRef>().unwrap();
    let bm = blocks.get(&txn, "r1").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(
        bm.get(&txn, "visibility"),
        Some(yrs::Out::Any("hidden".into()))
    );
    let _ = root;
}

// ---------------------------------------------------------------------------
// 12. ViewStore 封装（§5.6）：apply_update 重放路径
// ---------------------------------------------------------------------------

#[test]
fn view_store_roundtrip() {
    let mut p = pair();
    let mut agg = Aggregator;
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "hi"))).applied);

    let store = YrsViewStore::new(&p.chat);
    let snapshot = store.encode_state_as_update();
    assert!(!snapshot.is_empty());

    // 新 doc 重放快照 → 视图等价。
    let doc2 = yrs::Doc::new();
    let store2 = YrsViewStore::new(&doc2);
    store2.apply_update(&snapshot).unwrap();
    let txn2 = doc2.transact();
    let root2 = chat_writer::root_map_read(&txn2).unwrap();
    let entries2 = root2.get(&txn2, "entries").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(entries2.len(&txn2), 1);
    let _ = root2;
}

// ---------------------------------------------------------------------------
// 13. SessionListResponse 全量同步（§6.3/§5.2）
// ---------------------------------------------------------------------------

#[test]
fn session_list_full_sync_removes_stale() {
    let mut p = pair();
    let mut agg = Aggregator;
    let sum = |id: &str, title: &str| acp_hub_proto::schema::SessionSummaryProjection {
        session_id: id.to_string(),
        title: title.to_string(),
        status: "completed".to_string(),
        updated_at: "2026-08-07T00:00:00Z".to_string(),
    };
    // 第一轮：s1/s2。
    assert!(agg
        .apply(
            &mut p,
            &ev("s1", 1, EventBody::SessionListResponse {
                entries: vec![sum("s1", "a"), sum("s2", "b")],
            })
        )
        .applied);
    // 第二轮：s1 变化、s2 缺失（旧条目删除）、s3 新增。
    assert!(agg
        .apply(
            &mut p,
            &ev("s1", 2, EventBody::SessionListResponse {
                entries: vec![sum("s1", "a2"), sum("s3", "c")],
            })
        )
        .applied);
    let txn = p.session.transact();
    let root = chat_writer::root_map_read(&txn).unwrap();
    let sessions = root.get(&txn, "sessions").unwrap().cast::<yrs::MapRef>().unwrap();
    let keys: std::collections::BTreeSet<&str> = sessions
        .iter(&txn)
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, ["s1", "s3"].into_iter().collect());
    let _ = root;
}

// ---------------------------------------------------------------------------
// 14. projection_version 递增（§5.3/§5.6）
// ---------------------------------------------------------------------------

#[test]
fn projection_version_increments_per_apply() {
    let mut p = pair();
    let mut agg = Aggregator;
    let read = |pair: &DocPair| {
        let txn = pair.chat.transact();
        chat_writer::root_map_read(&txn)
            .and_then(|root| root.get(&txn, "projection_version"))
            .and_then(|v| v.cast::<u32>().ok())
            .unwrap_or(0)
    };
    assert_eq!(read(&p), 0);
    assert!(agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a"))).applied);
    assert_eq!(read(&p), 1);
    assert!(agg.apply(&mut p, &ev("s1", 2, msg_delta("t1", "t1:assistant", "b1", "x"))).applied);
    assert_eq!(read(&p), 2);
    // 拒绝的事件不 bump。
    let r = agg.apply(&mut p, &ev("s1", 1, user_msg("t1", "t1:user", "a")));
    assert!(!r.applied);
    assert_eq!(read(&p), 2);
}

#[test]
fn session_status_str_matches_schema() {
    assert_eq!(crate::state::aggregator::session_status_str(SessionStatus::Active), "active");
    assert_eq!(crate::state::aggregator::session_status_str(SessionStatus::Crashed), "crashed");
}
