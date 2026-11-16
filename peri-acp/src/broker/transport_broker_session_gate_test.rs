//! session-owned 交互门（`SessionState.interaction_gate`）的归属语义测试。
//!
//! 从 `transport_broker_test.rs` 拆出以满足 STD-SIZE-001；沿用其 `tests` 模块的
//! fixture 与 helper（`use super::*`）。

use crate::broker::transport_broker::tests::CausalFixture;
use crate::broker::transport_broker::AcpTransportBroker;
use agent_client_protocol::schema::v1::SessionId;
use peri_acp_types::interaction::InteractionResponse;
use peri_acp_types::interaction::UserInteractionBroker;
use std::sync::Arc;
use tokio::sync::oneshot;

#[tokio::test]
async fn test_interaction_gate_is_shared_by_same_session_brokers() {
    // 门的归属是 session：同一 session 的**不同轮 broker** 借用同一把门，
    // 跨轮同样互斥（旧实现每轮新建门 ⇒ 跨轮不互斥，本测试锁住修复）。
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = crate::broker::transport_broker::tests::causal_fixture();
    let gate = crate::broker::transport_broker::tests::interaction_gate();
    let broker_a = Arc::new(AcpTransportBroker::new(
        transport.clone(),
        SessionId::new("session-a"),
        Arc::clone(&gate),
    ));
    let broker_b = Arc::new(AcpTransportBroker::new(
        transport,
        SessionId::new("session-a"),
        Arc::clone(&gate),
    ));

    let first = tokio::spawn(async move {
        broker_a
            .request(
                crate::broker::transport_broker::tests::owner(),
                crate::broker::transport_broker::tests::approval_context(),
            )
            .await
    });
    assert_eq!(
        crate::broker::transport_broker::tests::next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let (pending_tx, pending_rx) = oneshot::channel();
    let second = tokio::spawn(
        crate::broker::transport_broker::tests::request_with_first_pending_marker(
            broker_b,
            crate::broker::transport_broker::tests::questions_context(),
            pending_tx,
        ),
    );
    pending_rx
        .await
        .expect("second broker must report its first Pending poll");
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "brokers of the same session must share one interaction gate"
    );

    releases.add_permits(1);
    assert!(matches!(
        first.await.unwrap(),
        InteractionResponse::Decisions(_)
    ));
    assert_eq!(
        crate::broker::transport_broker::tests::next_entered(&mut entered_rx).await,
        "elicitation/create"
    );
    releases.add_permits(1);
    assert!(matches!(
        second.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
}

#[tokio::test]
async fn test_interaction_gate_is_not_global_across_sessions() {
    // 门是 session 级，不是全局：不同 session 的交互互不阻塞。
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = crate::broker::transport_broker::tests::causal_fixture();
    let broker_a = Arc::new(AcpTransportBroker::new(
        transport.clone(),
        SessionId::new("session-a"),
        crate::broker::transport_broker::tests::interaction_gate(),
    ));
    let broker_b = Arc::new(AcpTransportBroker::new(
        transport,
        SessionId::new("session-b"),
        crate::broker::transport_broker::tests::interaction_gate(),
    ));

    let first = tokio::spawn(async move {
        broker_a
            .request(
                crate::broker::transport_broker::tests::owner(),
                crate::broker::transport_broker::tests::approval_context(),
            )
            .await
    });
    assert_eq!(
        crate::broker::transport_broker::tests::next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let (pending_tx, pending_rx) = oneshot::channel();
    let second = tokio::spawn(
        crate::broker::transport_broker::tests::request_with_first_pending_marker(
            broker_b,
            crate::broker::transport_broker::tests::questions_context(),
            pending_tx,
        ),
    );
    pending_rx
        .await
        .expect("second session broker must not wait on another session's gate");
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "different sessions must not share a global interaction gate"
    );
    assert_eq!(
        crate::broker::transport_broker::tests::next_entered(&mut entered_rx).await,
        "elicitation/create"
    );

    releases.add_permits(2);
    assert!(matches!(
        first.await.unwrap(),
        InteractionResponse::Decisions(_)
    ));
    assert!(matches!(
        second.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
}
