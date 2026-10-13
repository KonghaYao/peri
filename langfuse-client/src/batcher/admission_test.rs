use super::*;
use crate::types::TraceBody;
use crate::IngestionEvent;
use std::time::Duration;
use tokio::sync::oneshot;

fn event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 测试预算：只用于通过准入的预算身份校验，不参与容量断言。
fn budget() -> Arc<MemoryBudget> {
    MemoryBudget::new(usize::MAX).unwrap()
}

fn telemetry(id: &str, budget: &Arc<MemoryBudget>) -> TelemetryEvent {
    TelemetryEvent::try_new(event(id), None, None, budget).unwrap()
}

async fn poll_pending<T>(future: std::pin::Pin<&mut impl std::future::Future<Output = T>>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn test_close_drains_committed_work_without_waiting_for_reserved_permit() {
    let budget = budget();
    let bytes = queued_bytes(&telemetry("committed", &budget), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::for_test(2, 256, bytes);
    admission
        .send(telemetry("committed", &budget))
        .await
        .unwrap();
    // 第二个生产者已取得队列槽位、仍等待字节额度：它尚未提交，不进入 drain 集合。
    let mut reserved = Box::pin(admission.send(telemetry("late", &budget)));
    poll_pending(reserved.as_mut()).await;
    admission.close();
    assert!(matches!(receiver.try_recv(), Some(BatcherCommand::Add(_))));
    assert!(
        receiver.try_recv().is_none(),
        "尚未提交的 permit 不进入 drain 集合"
    );
    assert!(matches!(reserved.await, Err(LangfuseError::ChannelClosed)));
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn test_cancelled_capacity_waiter_cannot_publish_and_returns_its_slot() {
    let budget = budget();
    let (admission, mut receiver, _) = Admission::for_test(1, 256, usize::MAX);
    admission
        .try_add(telemetry("first", &budget), BackpressurePolicy::Block)
        .unwrap();
    let mut pending = Box::pin(admission.send(telemetry("cancelled", &budget)));
    poll_pending(pending.as_mut()).await;
    drop(pending);
    assert!(matches!(
        receiver.recv().await,
        Some(BatcherCommand::Add(_))
    ));
    admission
        .try_add(telemetry("next", &budget), BackpressurePolicy::Block)
        .unwrap();
    let Some(BatcherCommand::Add(QueuedEvent {
        event:
            TelemetryEvent {
                event: IngestionEvent::TraceCreate { id, .. },
                ..
            },
        ..
    })) = receiver.try_recv()
    else {
        panic!("必须保留后续成功准入事件")
    };
    assert_eq!(id, "next");
    assert!(receiver.try_recv().is_none(), "已取消的等待者不得迟到提交");
}

#[tokio::test]
async fn test_receiver_drop_releases_flush_ack_and_blocked_producer() {
    let budget = budget();
    let (admission, receiver, _) = Admission::for_test(1, 256, usize::MAX);
    let (ack, response) = oneshot::channel();
    admission.flush_command(ack).await.unwrap();
    let mut pending = Box::pin(admission.send(telemetry("blocked", &budget)));
    poll_pending(pending.as_mut()).await;
    drop(receiver);
    assert!(
        response.await.is_err(),
        "worker 消失必须释放已入队的 flush ack"
    );
    assert!(matches!(pending.await, Err(LangfuseError::ChannelClosed)));
    assert!(matches!(
        admission.try_add(telemetry("late", &budget), BackpressurePolicy::DropOldest),
        Err(LangfuseError::ChannelClosed)
    ));
}

#[tokio::test]
async fn test_cancelled_receive_does_not_lose_future_notification() {
    let budget = budget();
    let (admission, mut receiver, _) = Admission::for_test(1, 256, usize::MAX);
    let mut pending = Box::pin(receiver.recv());
    poll_pending(pending.as_mut()).await;
    drop(pending);
    admission
        .try_add(telemetry("later", &budget), BackpressurePolicy::DropNew)
        .unwrap();
    let command = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("取消 recv 后下一次等待仍应被唤醒");
    assert!(matches!(command, Some(BatcherCommand::Add(_))));
}

// ─── 冻结登记的线性化点（契约 §3.6） ─────────────────────────────────

/// 冻结来源的最小实现：只提供 retained 上界；准备任务在本次用例里不会被派发。
struct Source;

impl SnapshotSource for Source {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        Ok(8)
    }

    fn write_json(&self, _out: &mut dyn std::io::Write) -> Result<(), LangfuseError> {
        Ok(())
    }
}

/// [并发反例] 序号分配与登记入队必须同处一个 `state` 临界区。
///
/// 旧实现先分配 `prepare_seq`、放开 `state`，再去准备队列排队：在这两步之间捕获的水位会覆盖
/// 一个尚未入队的登记，flush 的前缀屏障据此误判为已收敛。这里占住准备队列把该交错固定下来：
/// 冻结等在准备队列上时**必须仍持有** `state`（水位因此不可能先于入队前进）。
#[tokio::test]
async fn freeze_holds_the_state_lock_until_its_registration_is_queued() {
    let budget = budget();
    let (admission, receiver, _) = Admission::for_test(2, 256, usize::MAX);
    let shared = Arc::clone(&admission.shared);
    let prepare_held = shared.prepare.lock().expect("batch prepare poisoned");

    let freeze = std::thread::spawn(move || {
        let seq = admission
            .try_freeze(Arc::new(Source), &budget, 16)
            .expect("冻结必须准入")
            .cell
            .prepare_seq;
        (seq, admission)
    });

    // 冻结此刻只能卡在准备队列上：线性化点要求它同时把 `state` 攥在手里。
    let mut holds_state = false;
    for _ in 0..2_000 {
        if shared.state.try_lock().is_err() {
            holds_state = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        holds_state,
        "序号分配与登记入队必须同处一个临界区：冻结等入队时不得放开 state"
    );

    drop(prepare_held);
    let (seq, admission) = freeze.join().expect("冻结线程正常结束");
    assert_eq!(seq, 1);

    // 水位只能覆盖已入队的前缀：捕获到水位时登记必须已经在队列里。
    let (ack, _response) = oneshot::channel();
    let boundary = admission.flush_command(ack).await.expect("flush 入队");
    assert_eq!(boundary.prepare_seq, 1, "水位必须覆盖刚登记的序号");
    let queued = receiver
        .take_prepare_upto(boundary.prepare_seq)
        .expect("水位覆盖的登记必须已在队列里");
    assert_eq!(queued.cell.prepare_seq, 1);
    assert!(
        !receiver.pending_prepare_upto(boundary.prepare_seq),
        "取走的登记不再留在队列里"
    );
}

/// [并发反例] 并发冻结的入队前缀必须与序号前缀一致：水位覆盖的序号不能有逆序或缺口。
///
/// 旧实现的两个锁段允许两个冻结按相反顺序入队，前缀屏障会漏掉水位之内、尚未入队的登记。
#[tokio::test]
async fn concurrent_freezes_register_a_gapless_ordered_prefix() {
    const FREEZERS: usize = 8;
    let expected: Vec<u64> = (1..=FREEZERS as u64).collect();
    for _round in 0..32 {
        let budget = budget();
        let (admission, receiver, _) = Admission::for_test(FREEZERS, 256, usize::MAX);
        let admission = Arc::new(admission);
        let barrier = Arc::new(std::sync::Barrier::new(FREEZERS));
        let mut freezers = Vec::new();
        for _ in 0..FREEZERS {
            let admission = Arc::clone(&admission);
            let budget = Arc::clone(&budget);
            let barrier = Arc::clone(&barrier);
            freezers.push(tokio::task::spawn_blocking(move || {
                barrier.wait();
                admission
                    .try_freeze(Arc::new(Source), &budget, 16)
                    .expect("并发冻结必须全部准入")
                    .cell
                    .prepare_seq
            }));
        }
        let mut allocated = Vec::new();
        for freezer in freezers {
            allocated.push(freezer.await.expect("冻结线程正常结束"));
        }

        let (ack, _response) = oneshot::channel();
        let boundary = admission.flush_command(ack).await.expect("flush 入队");
        assert_eq!(
            boundary.prepare_seq, FREEZERS as u64,
            "每个冻结恰好一个序号"
        );
        let mut queued = Vec::new();
        while let Some(request) = receiver.take_prepare_upto(boundary.prepare_seq) {
            queued.push(request.cell.prepare_seq);
        }
        assert_eq!(
            queued, expected,
            "水位前缀必须按序号 FIFO 入队：无逆序、无缺口"
        );
        allocated.sort_unstable();
        assert_eq!(allocated, expected, "每个冻结恰好一个序号");
    }
}
