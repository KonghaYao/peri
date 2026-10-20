//! 使用真实有界广播验证运行中和尾部 drain 的 lag 恢复。
use super::*;
use peri_acp_types::event_v2::{BusHealth, BusHealthSnapshot};
use tokio::sync::broadcast;

struct LagSubscriber {
    rx: broadcast::Receiver<EventMessage>,
    health: Arc<BusHealth>,
    drain_only: bool,
}

#[async_trait]
impl EventSubscriber for LagSubscriber {
    fn health_handle(&self) -> Option<Arc<BusHealth>> {
        Some(self.health.clone())
    }

    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        if self.drain_only {
            return std::future::pending().await;
        }
        self.rx.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(n) => SubscriptionError::Lagged(n),
            broadcast::error::RecvError::Closed => SubscriptionError::Closed,
        })
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        match self.rx.try_recv() {
            Ok(message) => Ok(Some(message)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Lagged(n)) => Err(SubscriptionError::Lagged(n)),
            Err(broadcast::error::TryRecvError::Closed) => Err(SubscriptionError::Closed),
        }
    }
}

async fn assert_lag_recovery(drain_only: bool) {
    let (mailbox, _) = mailbox_with_claimed_input();
    let (tx, rx) = broadcast::channel(1);
    let health = Arc::new(BusHealth::default());
    for chunk in ["被覆盖", "保留尾事件"] {
        tx.send(sequenced_message(
            SessionSeq::initial(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: chunk.into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    }
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox,
        Box::new(LagSubscriber {
            rx,
            health: health.clone(),
            drain_only,
        }),
        sink.clone(),
    );
    finish(event_tx, stop_tx, handle).await;
    assert_eq!(
        health.snapshot(),
        BusHealthSnapshot {
            render_dropped: 0,
            state_dropped: 1,
            dirty: true,
        }
    );
    let records = sink.records();
    assert!(
        records
            .iter()
            .any(|r| r.starts_with("other:UserInputQueueChanged")),
        "必须补真实邮箱快照"
    );
    assert!(
        records.iter().any(|r| r == "text:保留尾事件"),
        "Lagged 后必须继续 drain"
    );
    assert!(!records.iter().any(|r| r == "text:被覆盖"));
    assert_eq!(records.last().map(String::as_str), Some("done"));
    assert!(
        health.snapshot().dirty,
        "局部快照补发不能证明整条事件链已恢复"
    );
    drop(tx);
}

#[tokio::test]
async fn active_subscription_lag_marks_health_and_refreshes_mailbox() {
    assert_lag_recovery(false).await;
}

#[tokio::test]
async fn trailing_lag_continues_draining_retained_events() {
    assert_lag_recovery(true).await;
}
