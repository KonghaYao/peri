//! `RequestLedger`（设计 §8 六种情况 + 指纹 / 容量 / 回收 / 销毁不可重建）。
//!
//! 全部用小预算触发边界（§9.2：不得把「1024 条目」当有界）；生产预算来自
//! `mcp_apps_facade` 的 §4.3 常量。

use std::time::Duration;

use peri_acp_types::mcp_apps::{NotExecutedCause, SessionInvocationOutcome};

use super::*;

fn operation(value: u64) -> RequestOperation {
    RequestOperation::AppToolsCall {
        record_id: "app-record".into(),
        tool_name: "show_canvas".into(),
        arguments: Map::from_iter([("value".into(), Value::from(value))]),
    }
}

fn fingerprint(value: u64) -> RequestFingerprint {
    RequestFingerprint::new("session-1", &operation(value))
}

fn tools_call(outcome: SessionInvocationOutcome) -> AppsRequestOutcome {
    AppsRequestOutcome::ToolsCall(outcome)
}

fn completed(text: &str) -> AppsRequestOutcome {
    tools_call(SessionInvocationOutcome::Completed(
        peri_acp_types::tool_outcome::RawToolResult::new(serde_json::json!({
            "content": [{"type": "text", "text": text}],
        })),
    ))
}

fn not_executed() -> AppsRequestOutcome {
    tools_call(not_executed_before_commit())
}

fn ledger(max_pending: usize, max_results: usize, max_bytes: usize) -> RequestLedger {
    RequestLedger::new(LedgerBudget::for_tests(
        8,
        max_pending,
        max_results,
        1 << 20,
        max_bytes,
    ))
}

fn namespace(ledger: &RequestLedger, name: &str) -> RequestNamespaceId {
    let id = RequestNamespaceId::from(name);
    ledger
        .ensure_namespace(&id, NamespaceKind::Attachment)
        .expect("测试 namespace 必须可打开");
    id
}

/// §8 第 2 行：同 key 同指纹 ⇒ 只登记一次，加入同一等待并取得同一结果。
#[tokio::test]
async fn same_seq_same_fingerprint_joins_pending_and_returns_same_result() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-join");

    let first = ledger.admit(&ns, RequestSeq::new(1), &fingerprint(7));
    let pending = match first {
        AdmissionVerdict::Accepted { pending } => pending,
        other => panic!("新序号必须被登记，实际: {other:?}"),
    };

    let second = ledger.admit(&ns, RequestSeq::new(1), &fingerprint(7));
    let joined = match second {
        AdmissionVerdict::JoinedPending { pending } => pending,
        other => panic!("同 key 同指纹必须加入同一等待，实际: {other:?}"),
    };
    assert!(
        Arc::ptr_eq(&pending, &joined),
        "同 key 必须共用同一 pending（只执行一次）"
    );
    assert!(
        Arc::ptr_eq(&pending.slot, &joined.slot),
        "同 key 必须共用同一结果槽"
    );
    assert_eq!(
        ledger.pending_len(&ns),
        1,
        "同 key 重发不得产生第二条 pending"
    );
    assert_eq!(
        ledger.next_seq(&ns),
        RequestSeq::new(2),
        "登记后推进权威 = last_accepted + 1"
    );

    assert!(
        ledger.complete(&ns, RequestSeq::new(1), completed("canvas ready")),
        "已登记请求的终态必须被账本记账"
    );
    let outcome = joined
        .slot
        .wait(Duration::from_millis(50))
        .await
        .expect("终态必须已发布");
    assert_eq!(outcome, completed("canvas ready"));
    assert_eq!(
        pending.slot.get().expect("同一槽"),
        completed("canvas ready")
    );

    // 终态之后同 key 再传：返回同一结果，不再执行。
    match ledger.admit(&ns, RequestSeq::new(1), &fingerprint(7)) {
        AdmissionVerdict::Duplicate { outcome } => {
            assert_eq!(outcome, completed("canvas ready"))
        }
        other => panic!("终态后同 key 必须返回同一结果，实际: {other:?}"),
    }
    assert_eq!(ledger.next_seq(&ns), RequestSeq::new(2));
    assert_eq!(ledger.pending_len(&ns), 0);
}

/// §8 第 3 行：同 key 但指纹不同 ⇒ `Conflict`（pending 与终态两种状态都要判）。
#[tokio::test]
async fn same_seq_different_fingerprint_conflicts() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-conflict");

    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::Accepted { .. }
    ));
    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(1), &fingerprint(2)),
            AdmissionVerdict::Conflict
        ),
        "同一序号的不同参数必须 Conflict（不得当作重发）"
    );

    ledger.complete(&ns, RequestSeq::new(1), completed("done"));
    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(1), &fingerprint(2)),
            AdmissionVerdict::Conflict
        ),
        "终态后的指纹不符同样是 Conflict，不得返回他人结果"
    );
}

/// §8 第 5 行：序号跳跃 ⇒ `SequenceGap`（返回期望序号）。
#[tokio::test]
async fn sequence_gap_reports_expected() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-gap");

    match ledger.admit(&ns, RequestSeq::new(5), &fingerprint(1)) {
        AdmissionVerdict::SequenceGap { expected } => {
            assert_eq!(expected, RequestSeq::new(1), "首次可用序号是 1")
        }
        other => panic!("跳跃必须 SequenceGap，实际: {other:?}"),
    }
    assert_eq!(ledger.pending_len(&ns), 0, "跳跃不得登记");
    assert_eq!(ledger.next_seq(&ns), RequestSeq::new(1), "跳跃不得消耗序号");

    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::Accepted { .. }
    ));
    match ledger.admit(&ns, RequestSeq::new(3), &fingerprint(1)) {
        AdmissionVerdict::SequenceGap { expected } => {
            assert_eq!(expected, RequestSeq::new(2))
        }
        other => panic!("跳跃必须 SequenceGap，实际: {other:?}"),
    }
    // 序号 0 不是可用序号：给期望值而不是「已回收」。
    match ledger.admit(&ns, RequestSeq::new(0), &fingerprint(1)) {
        AdmissionVerdict::SequenceGap { expected } => {
            assert_eq!(expected, RequestSeq::new(2))
        }
        other => panic!("非法序号必须给期望值，实际: {other:?}"),
    }
}

/// §8 第 4 行：结果已回收 ⇒ `ResultExpired`，**绝不**当作新执行。
#[tokio::test]
async fn expired_result_is_never_reexecuted() {
    let ledger = ledger(4, 1, 1 << 20);
    let ns = namespace(&ledger, "ns-expired");

    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::Accepted { .. }
    ));
    ledger.complete(&ns, RequestSeq::new(1), completed("first"));
    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(2), &fingerprint(2)),
        AdmissionVerdict::Accepted { .. }
    ));
    ledger.complete(&ns, RequestSeq::new(2), completed("second"));
    assert_eq!(ledger.retained_len(&ns), 1, "条数预算回收最旧终态");

    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
            AdmissionVerdict::ResultExpired
        ),
        "结果已回收：即使指纹相同也必须 ResultExpired"
    );
    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(1), &fingerprint(9)),
            AdmissionVerdict::ResultExpired
        ),
        "结果已回收：无法再比较指纹，也绝不当作新执行"
    );
    assert_eq!(
        ledger.next_seq(&ns),
        RequestSeq::new(3),
        "已回收的旧序号不得被重新启用"
    );

    // 字节预算同样触发回收（单条超限 = 显式不可用，但保留防重事实）。
    let bytes_bounded = RequestLedger::new(LedgerBudget::for_tests(8, 4, 4, 8, 1 << 20));
    let ns = namespace(&bytes_bounded, "ns-bytes");
    assert!(matches!(
        bytes_bounded.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::Accepted { .. }
    ));
    bytes_bounded.complete(&ns, RequestSeq::new(1), completed("a longer payload"));
    assert_eq!(bytes_bounded.retained_bytes(), 0, "超单条上限不保留结果");
    assert!(
        matches!(
            bytes_bounded.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
            AdmissionVerdict::ResultExpired
        ),
        "超限结果必须显式不可用（同一个不可再执行的事实）"
    );
    assert!(
        matches!(
            bytes_bounded.admit(&ns, RequestSeq::new(1), &fingerprint(2)),
            AdmissionVerdict::Conflict
        ),
        "防重事实保留：指纹仍可比较 ⇒ 不同指纹是 Conflict"
    );
}

/// §8 第 6 行：容量不足且尚未登记 ⇒ `CapacityRejected`，**不消耗序号**。
#[tokio::test]
async fn capacity_rejection_does_not_consume_seq() {
    let ledger = RequestLedger::new(LedgerBudget::for_tests(4, 1, 4, 1 << 20, 1 << 20));
    let ns = namespace(&ledger, "ns-capacity");

    let first = ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1));
    let pending = match first {
        AdmissionVerdict::Accepted { pending } => pending,
        other => panic!("首个请求必须被登记，实际: {other:?}"),
    };
    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(2), &fingerprint(2)),
            AdmissionVerdict::CapacityRejected
        ),
        "pending 上限（= 1）达到后新请求必须 CapacityRejected"
    );
    assert_eq!(
        ledger.next_seq(&ns),
        RequestSeq::new(2),
        "容量拒绝不得消耗序号"
    );
    assert_eq!(ledger.pending_len(&ns), 1);
    assert!(!pending.cancel.is_cancelled(), "容量拒绝与取消无关");

    // 容量释放（终态）后同一序号可以登记。
    ledger.complete(&ns, RequestSeq::new(1), completed("first"));
    assert!(
        matches!(
            ledger.admit(&ns, RequestSeq::new(2), &fingerprint(2)),
            AdmissionVerdict::Accepted { .. }
        ),
        "容量释放后原序号必须可登记"
    );

    // namespace 条数上限同样是显式拒绝，不静默复用。
    let one = RequestLedger::new(LedgerBudget::for_tests(1, 4, 4, 1 << 20, 1 << 20));
    let _ = namespace(&one, "ns-one");
    assert_eq!(
        one.ensure_namespace(
            &RequestNamespaceId::from("ns-two"),
            NamespaceKind::Attachment
        ),
        Err(NamespaceOpenError::CapacityExceeded)
    );
}

/// A16（同进程部分）：销毁的 namespace 不得以同 ID 重建，同 ID 请求零登记零执行。
#[tokio::test]
async fn destroyed_namespace_cannot_be_recreated_with_same_id() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-destroyed");
    let pending = match ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)) {
        AdmissionVerdict::Accepted { pending } => pending,
        other => panic!("登记失败: {other:?}"),
    };
    assert_eq!(ledger.namespace_len(), 1);

    ledger.destroy(&ns);

    assert_eq!(ledger.namespace_len(), 0, "销毁必须释放 namespace 槽位");
    assert!(
        pending.cancel.is_cancelled(),
        "销毁必须发出准入前放弃信号（在飞执行有界收尾）"
    );
    assert_eq!(
        pending.cancel_reason(),
        Some(RequestCancelReason::AttachmentReplaced),
        "放弃原因必须如实登记"
    );
    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::UnknownNamespace
    ));
    assert!(matches!(
        ledger.admit(&ns, RequestSeq::new(2), &fingerprint(1)),
        AdmissionVerdict::UnknownNamespace
    ));
    assert_eq!(
        ledger.next_seq(&ns),
        RequestSeq::new(1),
        "销毁后不保留任何推进事实"
    );
    assert_eq!(
        ledger.ensure_namespace(&ns, NamespaceKind::Attachment),
        Err(NamespaceOpenError::Destroyed),
        "同 ID 不得重建"
    );
    assert_eq!(
        ledger.cancel(
            &ns,
            RequestSeq::new(1),
            RequestCancelReason::ClientCancelled
        ),
        CancelVerdict::Unknown
    );
    assert!(ledger.slot(&ns, RequestSeq::new(1)).is_none());

    // 新 namespace（新 ID）照常可用。
    let fresh = namespace(&ledger, "ns-fresh");
    assert!(matches!(
        ledger.admit(&fresh, RequestSeq::new(1), &fingerprint(1)),
        AdmissionVerdict::Accepted { .. }
    ));
    assert_eq!(ledger.namespace_len(), 1);
}

/// 销毁痕迹有界（FIFO tombstone）：不得因长期会话而无界增长。
#[tokio::test]
async fn destroyed_namespace_tombstones_are_bounded() {
    let ledger = RequestLedger::new(LedgerBudget::for_tests(1, 4, 4, 1 << 20, 1 << 20));
    for index in 0..(DESTROYED_TOMBSTONES + 16) {
        let ns = namespace(&ledger, &format!("ns-tomb-{index}"));
        ledger.destroy(&ns);
    }
    assert!(
        ledger.destroyed_len() <= DESTROYED_TOMBSTONES,
        "销毁痕迹必须有界，实际: {}",
        ledger.destroyed_len()
    );
}

/// D7 边界：未决请求的取消如实登记原因，且不产出伪造的终态。
#[tokio::test]
async fn cancel_records_reason_and_never_fabricates_terminal_outcome() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-cancel");
    let first = match ledger.admit(&ns, RequestSeq::new(1), &fingerprint(1)) {
        AdmissionVerdict::Accepted { pending } => pending,
        other => panic!("登记失败: {other:?}"),
    };
    let second = match ledger.admit(&ns, RequestSeq::new(2), &fingerprint(2)) {
        AdmissionVerdict::Accepted { pending } => pending,
        other => panic!("登记失败: {other:?}"),
    };

    assert_eq!(
        ledger.cancel(
            &ns,
            RequestSeq::new(1),
            RequestCancelReason::ClientCancelled
        ),
        CancelVerdict::Accepted
    );
    assert!(first.cancel.is_cancelled());
    assert_eq!(
        first.cancel_reason(),
        Some(RequestCancelReason::ClientCancelled)
    );
    assert_eq!(
        first.slot.get(),
        None,
        "取消只发出放弃信号，不得伪造终态（终态由执行方 / 从未 dispatch 的显式未执行产出）"
    );

    assert_eq!(
        ledger.cancel(
            &ns,
            RequestSeq::new(2),
            RequestCancelReason::AttachmentReplaced
        ),
        CancelVerdict::Accepted
    );
    assert!(second.cancel.is_cancelled());
    assert_eq!(
        second.cancel_reason(),
        Some(RequestCancelReason::AttachmentReplaced)
    );
    assert_eq!(
        ledger.cancel(
            &ns,
            RequestSeq::new(9),
            RequestCancelReason::ClientCancelled
        ),
        CancelVerdict::Unknown,
        "未登记序号不得被「取消」成已受理"
    );

    ledger.complete(&ns, RequestSeq::new(1), not_executed());
    assert_eq!(
        ledger.cancel(
            &ns,
            RequestSeq::new(1),
            RequestCancelReason::ClientCancelled
        ),
        CancelVerdict::AlreadyFinished
    );

    // 整 namespace 取消（附件替换）：返回全部**未决**请求供执行方收尾。
    // seq=1 已终结（终态在上方 complete），seq=2 仍是未决（cancel 只发放弃信号，
    // 终态由执行方产出）⇒ 本次返回的正是它。
    let replaced = ledger.cancel_pending(&ns, RequestCancelReason::AttachmentReplaced);
    assert_eq!(replaced.len(), 1, "整 namespace 取消只作用于仍未决的请求");
    assert!(replaced[0].cancel.is_cancelled());
    assert_eq!(
        replaced[0].cancel_reason(),
        Some(RequestCancelReason::AttachmentReplaced)
    );
}

/// 指纹覆盖参数与目标，且对键序不敏感（规范化 JSON）。
#[test]
fn fingerprint_covers_arguments_and_ignores_key_order() {
    let left = RequestOperation::AppToolsCall {
        record_id: "record".into(),
        tool_name: "show".into(),
        arguments: Map::from_iter([("a".into(), Value::from(1)), ("b".into(), Value::from(2))]),
    };
    let right = RequestOperation::AppToolsCall {
        record_id: "record".into(),
        tool_name: "show".into(),
        arguments: Map::from_iter([("b".into(), Value::from(2)), ("a".into(), Value::from(1))]),
    };
    let same = RequestFingerprint::new("session-1", &left);
    assert_eq!(
        same,
        RequestFingerprint::new("session-1", &right),
        "键序不改变业务身份"
    );

    let other_arguments = RequestOperation::AppToolsCall {
        record_id: "record".into(),
        tool_name: "show".into(),
        arguments: Map::from_iter([("a".into(), Value::from(2))]),
    };
    assert_ne!(
        same,
        RequestFingerprint::new("session-1", &other_arguments),
        "参数变化必须改变指纹"
    );

    let other_target = RequestOperation::HostToolsCall {
        server_id: "server-1".into(),
        tool_name: "show".into(),
        arguments: Map::new(),
    };
    assert_ne!(
        same,
        RequestFingerprint::new("session-1", &other_target),
        "目标类别变化必须改变指纹"
    );
    assert_ne!(
        same,
        RequestFingerprint::new("session-2", &left),
        "跨会话不得共用身份"
    );

    let nested_a = RequestOperation::ResourcesList {
        record_id: "record".into(),
        cursor: Some("c1".into()),
    };
    let nested_b = RequestOperation::ResourcesList {
        record_id: "record".into(),
        cursor: Some("c1".into()),
    };
    assert_eq!(
        RequestFingerprint::new("session-1", &nested_a),
        RequestFingerprint::new("session-1", &nested_b)
    );
    assert_ne!(
        RequestFingerprint::new("session-1", &nested_a),
        RequestFingerprint::new("session-1", &left)
    );
}

/// 「从未成为 invocation」的显式未执行事实：唯一判决是 `CancelledBeforeCommit`。
#[test]
fn not_executed_before_commit_is_cancelled_before_commit() {
    assert_eq!(
        not_executed_before_commit(),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::CancelledBeforeCommit
        }
    );
    assert_ne!(
        not_executed_before_commit(),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UserRejected
        },
        "未获批准 / 被替换不得呈现为「用户拒绝」"
    );
}

/// namespace 类别不符与预算来源（生产预算即 §4.3 常量投影）。
#[test]
fn namespace_kind_mismatch_is_rejected_and_budget_follows_contract_constants() {
    let ledger = ledger(4, 4, 1 << 20);
    let ns = namespace(&ledger, "ns-kind");
    assert_eq!(
        ledger.ensure_namespace(&ns, NamespaceKind::Host),
        Err(NamespaceOpenError::KindMismatch)
    );
    let budget = ledger.budget_for_tests();
    assert_eq!(
        budget.max_result_bytes,
        1 << 20,
        "测试预算显式给出，生产预算另由 conservative() 引用契约常量"
    );
    let production = LedgerBudget::conservative();
    assert_eq!(
        production.max_namespaces,
        MAX_NAMESPACES_PER_SESSION as usize
    );
    assert_eq!(
        production.max_pending_per_namespace,
        MAX_PENDING_PER_NAMESPACE as usize
    );
    assert_eq!(
        production.max_retained_results,
        MAX_RETAINED_RESULTS as usize
    );
    assert_eq!(production.max_result_bytes, MAX_RESULT_BYTES as usize);
    assert_eq!(production.max_retained_bytes, MAX_RETAINED_BYTES as usize);
}
