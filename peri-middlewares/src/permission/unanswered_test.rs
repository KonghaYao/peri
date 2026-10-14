//! 「无人可作答」（[`InteractionResponse::Unanswered`]）的审批回归测试。
//!
//! 语义要求（设计 §4.3）：无在线审批 endpoint 时审批**未获批准 ⇒ 操作未执行**，
//! 原因必须**如实转述**，不得伪装成「用户拒绝」；typed 原因（裁决 R9）必须随
//! `AgentError::ToolRejected` 的 `cause` 过边界，消费方无需带外记录点。
//!
//! 本文件同时升级一处旧「现状钉死」用例：R9 之前两类事实共享同一 typed 失败
//!（只在诊断文本里可区分），现在必须 typed 可区分（见文件末尾用例）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_agent::agent::state::AgentState;
use peri_agent::interaction::UnansweredCause;

use super::*;
use peri_acp_types::error::ToolRejectionCause;

/// 恒定返回「无人可作答」的 broker，并记录交互次数。
struct UnansweredBroker {
    cause: UnansweredCause,
    calls: AtomicUsize,
}

impl UnansweredBroker {
    fn new(cause: UnansweredCause) -> Arc<Self> {
        Arc::new(Self {
            cause,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl UserInteractionBroker for UnansweredBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        InteractionResponse::Unanswered { cause: self.cause }
    }
}

/// 有归属的审批态：交互必须真的发出（否则「零交互」会掩盖未审批）。
fn state_with_owner() -> AgentState {
    AgentState::new("/tmp").with_turn_identity(
        "session-test",
        peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    )
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall::new(id, name, serde_json::json!({}))
}

/// 拒绝原因必须**逐字**等于 cause 的转述，且不是「用户拒绝」；typed 原因必须
/// 如实携带（裁决 R9：不再是「只在诊断文本里可区分」）。
fn assert_transcribed_rejection(result: &AgentResult<ToolCall>, cause: UnansweredCause) {
    match result {
        Err(AgentError::ToolRejected {
            reason,
            cause: rejection_cause,
            ..
        }) => {
            assert_eq!(
                reason,
                cause.reason_text(),
                "无人可作答必须如实转述原因，不得改写"
            );
            assert_ne!(
                reason.as_str(),
                "用户拒绝",
                "不得把「无人可作答」压成「用户拒绝」"
            );
            assert_eq!(
                *rejection_cause,
                ToolRejectionCause::ApprovalUnavailable(cause),
                "「没有人可以决定」必须以 typed 原因过边界，不得退化成用户拒绝"
            );
        }
        other => panic!("未获批准必须是显式失败，实际: {other:?}"),
    }
}

/// [回归] 单条审批（`broker_approve`）：无在线 endpoint ⇒ 如实转述 + 未获批准。
#[tokio::test]
async fn single_approval_transcribes_no_online_endpoint() {
    let broker = UnansweredBroker::new(UnansweredCause::NoOnlineEndpoint);
    let mw = PermissionMiddleware::new(broker.clone(), default_requires_approval);
    let mut state = state_with_owner();

    let result = mw.before_tool(&mut state, &call("c1", "Write")).await;

    assert_eq!(broker.calls(), 1, "审批必须真的发出过一次交互");
    assert_transcribed_rejection(&result, UnansweredCause::NoOnlineEndpoint);
    let Err(AgentError::ToolRejected { reason, .. }) = &result else {
        unreachable!("上面已断言为显式拒绝");
    };
    assert!(
        reason.contains("在线的审批 endpoint"),
        "原因必须点明「无在线审批 endpoint」这一事实: {reason}"
    );
    assert!(
        reason.contains("未执行"),
        "原因必须说明操作未执行（不是「已拒绝并放行」）: {reason}"
    );
}

/// [回归] 单条审批：客户端自述无交互能力（`-p` 打印模式）同样如实转述。
#[tokio::test]
async fn single_approval_transcribes_non_interactive_client() {
    let broker = UnansweredBroker::new(UnansweredCause::NonInteractiveClient);
    let mw = PermissionMiddleware::new(broker.clone(), default_requires_approval);
    let mut state = state_with_owner();

    let result = mw.before_tool(&mut state, &call("c1", "Write")).await;

    assert_eq!(broker.calls(), 1);
    assert_transcribed_rejection(&result, UnansweredCause::NonInteractiveClient);
}

/// [回归] 批量审批（`batch_broker_approve`）：无人可作答时**逐条**如实转述，
/// 不需要审批的调用不受影响（结果与输入等长）。
#[tokio::test]
async fn batch_approval_transcribes_no_online_endpoint_per_pending_call() {
    let broker = UnansweredBroker::new(UnansweredCause::NoOnlineEndpoint);
    let mw = PermissionMiddleware::new(broker.clone(), default_requires_approval);
    let mut state = state_with_owner();

    let calls = vec![call("c1", "Write"), call("c2", "Read")];
    let results = mw.before_tools_batch(&mut state, &calls).await;

    assert_eq!(broker.calls(), 1, "同一批次恰好一次交互");
    assert_eq!(results.len(), calls.len(), "结果必须与输入等长");
    assert_transcribed_rejection(&results[0], UnansweredCause::NoOnlineEndpoint);
    assert!(
        results[1].is_ok(),
        "不需要审批的调用不受无人可作答影响，实际: {:?}",
        results[1]
    );
}

/// R9 之前本用例钉死的是**现状**（不是期望语义）；R9 之后改为期望语义：
/// typed 失败原因**必须**区分「无人可答」与「用户拒绝」。
///
/// R9 之前：两条路径都收敛到 `AgentError::ToolRejected`，执行层统一映射
/// `UserRejected`，可区分性只在诊断文本里（旧用例名
/// `unanswered_and_user_rejection_share_one_typed_failure`）。
/// R9 之后：typed cause 必须不同，且**不得**再共享同一失败分类。
#[tokio::test]
async fn unanswered_and_user_rejection_are_typed_distinct() {
    let unanswered = UnansweredBroker::new(UnansweredCause::NoOnlineEndpoint);
    let mw = PermissionMiddleware::new(unanswered, default_requires_approval);
    let mut state = state_with_owner();
    let from_unanswered = mw
        .before_tool(&mut state, &call("c1", "Write"))
        .await
        .expect_err("无人可作答必须显式失败");

    struct RejectingBroker;
    #[async_trait]
    impl UserInteractionBroker for RejectingBroker {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            _ctx: InteractionContext,
        ) -> InteractionResponse {
            InteractionResponse::Decisions(vec![ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            }])
        }
    }
    let mw = PermissionMiddleware::new(Arc::new(RejectingBroker), default_requires_approval);
    let mut state = state_with_owner();
    let from_rejection = mw
        .before_tool(&mut state, &call("c1", "Write"))
        .await
        .expect_err("用户拒绝必须显式失败");

    let (
        AgentError::ToolRejected {
            cause: unanswered_cause,
            ..
        },
        AgentError::ToolRejected {
            cause: rejection_cause,
            ..
        },
    ) = (&from_unanswered, &from_rejection)
    else {
        panic!("两条路径都必须是显式 ToolRejected 失败");
    };
    assert_eq!(
        *unanswered_cause,
        ToolRejectionCause::ApprovalUnavailable(UnansweredCause::NoOnlineEndpoint),
        "无人可答必须带 ApprovalUnavailable(NoOnlineEndpoint)"
    );
    assert_eq!(
        *rejection_cause,
        ToolRejectionCause::UserRejected,
        "用户拒绝必须带 UserRejected"
    );
    assert_ne!(
        unanswered_cause, rejection_cause,
        "两类事实在类型层必须可区分（R9 的移除条件）"
    );
    assert_ne!(
        from_unanswered.to_string(),
        from_rejection.to_string(),
        "诊断文本同样必须仍能区分二者"
    );
}
