//! 真实 SQLite 上的 sync 交接结算：交接回执走到 finalize 与父 MQ。
//!
//! 既有用例分别覆盖「执行体/worker 语义（mock store）」与「本机存储事务（peri-resources）」；
//! 这里把两侧接起来，验证交接终态、工作意图与父 MQ 结果在真实本机存储上共同收敛。
//! 远端 adapter 的测试夹具不跨 crate 暴露，`远端 + resume + sync handoff` 的组合边界
//! 仍由 `peri-resources` 的远端结算用例与本文件的 mock 对应用例分别覆盖。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::async_tasks::TaskManager;
use crate::agent::events::BackgroundTaskResult;
use crate::session::subagent::factory::ResumeClaim;
use crate::session::subagent::SubagentHost;
use crate::session::test_resources::TestSession;
use crate::session::{FrozenContext, Session};

struct Fixture {
    _session: TestSession,
    resources: Arc<dyn SessionResources>,
    parent: ThreadId,
    child: ThreadId,
    parent_queue: MessageQueue,
    manager: Arc<TaskManager>,
    task_id: String,
    route: TerminalMqRoute,
}

impl Fixture {
    async fn open() -> Self {
        let session = TestSession::open().await;
        let resources = session.resources();
        let parent = session.thread_id();
        let child = session.create_child(&parent).await;
        let parent_queue = MessageQueue::bind(Arc::clone(&resources), parent.clone());
        let manager = Arc::new(TaskManager::new());
        let task_id = new_sync_task_id();
        let route = TerminalMqRoute {
            store: Arc::clone(&resources),
            target: parent.clone(),
            mailbox: None,
        };
        Self {
            _session: session,
            resources,
            parent,
            child,
            parent_queue,
            manager,
            task_id,
            route,
        }
    }

    fn child_session(&self) -> Arc<Session> {
        let session = Session::new_with_cancel_and_queue(
            Arc::from("/tmp"),
            FrozenContext::builder().build(),
            Some(self.child.clone()),
            Arc::new(CancellationToken::new()),
            MessageQueue::bind(Arc::clone(&self.resources), self.child.clone()),
        );
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard)
            .with_persistence(Arc::clone(&self.resources), self.child.clone());
        drop(guard);
        session.set_subagent_host(SubagentHost {
            session_resources: Some(Arc::clone(&self.resources)),
            ..Default::default()
        });
        session
    }
}

fn receipt(session: &Arc<Session>, child: &ThreadId) -> ChildHandoffReceipt {
    ChildHandoffReceipt {
        child_thread_id: child.as_str().to_owned(),
        execution: session.start_turn().execution_binding(),
        owner_session_id: child.as_str().to_owned(),
        reason: ChildHandoffReason::SettlementDeadline,
        tasks: Vec::new(),
        message_ids: Vec::new(),
        message_sequences: Vec::new(),
        unprocessed_tool_calls: Vec::new(),
        canonical_message_id: peri_acp_types::messages::MessageId::new(),
    }
}

async fn wait_for_terminal(
    fixture: &Fixture,
    delivered: &Arc<parking_lot::Mutex<Option<BackgroundTaskResult>>>,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let queued = fixture.parent_queue.snapshot().await.unwrap();
            let meta = fixture
                .resources
                .load_session_meta(&fixture.child)
                .await
                .unwrap();
            // 终态与 MQ 先于 registry 交付可见；同时等到回调，避免测试断言踩交付竞态。
            if queued.len() == 1 && meta.agent_status.is_terminal() && delivered.lock().is_some() {
                break;
            }
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("handoff worker must settle against the real store");
}

/// 同步执行在交接后结束：finalize 在真实 SQLite 上写终态、父 MQ 与接收方意图。
#[tokio::test]
async fn sqlite_sync_handoff_finalizes_child_and_parent_mq_end_to_end() {
    let fixture = Fixture::open().await;
    let session = fixture.child_session();
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = Arc::clone(&delivered);
    let receipt = receipt(&session, &fixture.child);

    let completion = run_sync_with_registered_handoff(
        Arc::clone(&session),
        fixture.child.as_str().to_owned(),
        "fixture".into(),
        Some(Arc::clone(&fixture.manager)),
        Some(fixture.task_id.clone()),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            *observed.lock() = Some(result.clone());
            Ok(())
        })),
        Some(fixture.route.clone()),
        move |handoff, _| async move {
            handoff
                .sender
                .send(receipt)
                .expect("receipt receiver alive");
            Ok(ChildRunOutcome::Settled)
        },
    )
    .await
    .unwrap();
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    wait_for_terminal(&fixture, &delivered).await;

    let meta = fixture
        .resources
        .load_session_meta(&fixture.child)
        .await
        .unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(
        fixture.parent_queue.snapshot().await.unwrap().len(),
        1,
        "父 MQ 必须恰好收到一条结果"
    );
    let parent = fixture
        .resources
        .load_session_meta(&fixture.parent)
        .await
        .unwrap();
    assert_eq!(
        parent.work_state,
        WorkState::Working,
        "结果接纳必须同事务提升接收方工作意图"
    );

    // 同一消息身份的重放是 no-op：真实存储的 Existing 归类不能产生第二条结果。
    let result = delivered.lock().clone().expect("delivery captured");
    fixture
        .route
        .settle(fixture.child.as_str(), AgentStatus::Done, &result)
        .await
        .unwrap();
    assert_eq!(
        fixture.parent_queue.snapshot().await.unwrap().len(),
        1,
        "重放不得产生第二条父 MQ 结果"
    );
    let meta = fixture
        .resources
        .load_session_meta(&fixture.child)
        .await
        .unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);
}

/// resume 认领 + sync 交接：认领保持 active，worker 的 finalize 在真实存储的
/// `active` 守卫下写入唯一终态，结果仍进入父 MQ。
#[tokio::test]
async fn sqlite_resumed_handoff_finalizes_under_active_guard() {
    let fixture = Fixture::open().await;
    fixture
        .resources
        .update_session_meta(
            &fixture.child,
            &peri_acp_types::session_resources::SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, claim) = ResumeClaim::acquire(
        Arc::clone(&fixture.resources),
        fixture.child.as_str().to_owned(),
        fixture.parent.as_str().to_owned(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        fixture
            .resources
            .load_session_meta(&fixture.child)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Active,
        "认领后、finalize 前必须保持 active"
    );

    let session = fixture.child_session();
    let delivered = Arc::new(parking_lot::Mutex::new(None));
    let observed = Arc::clone(&delivered);
    let receipt = receipt(&session, &fixture.child);
    let completion = run_sync_with_registered_handoff(
        Arc::clone(&session),
        fixture.child.as_str().to_owned(),
        "fixture".into(),
        Some(Arc::clone(&fixture.manager)),
        Some(fixture.task_id.clone()),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            *observed.lock() = Some(result.clone());
            Ok(())
        })),
        Some(fixture.route.clone()),
        move |handoff, _| async move {
            handoff
                .sender
                .send(receipt.clone())
                .expect("receipt receiver alive");
            // 与生产交接一致：终态归 handoff worker，认领只解除自身的结算责任。
            claim.release_without_terminal().await.unwrap();
            Ok(ChildRunOutcome::HandedOff { receipt })
        },
    )
    .await
    .unwrap();
    assert!(matches!(completion, SyncCompletion::Pending(_)));
    wait_for_terminal(&fixture, &delivered).await;

    let meta = fixture
        .resources
        .load_session_meta(&fixture.child)
        .await
        .unwrap();
    // HandedOff 是「委派未完成」：按 A3 映射 Error + Idle，而不是伪造 Done。
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(fixture.parent_queue.snapshot().await.unwrap().len(), 1);
    assert!(delivered.lock().is_some(), "worker 必须完成父结果交付");
}
