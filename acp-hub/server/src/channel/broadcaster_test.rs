//! Broadcaster 单测（设计稿 §16 测试 18–19）。

use serde_json::json;
use tokio::sync::mpsc;

use acp_hub_proto::conn::DocId;
use acp_hub_proto::frame::Frame;

use super::*;

/// 构造一个内容确定性的 y-sync update（非空、编码合法；`payload` 控制
/// update 字节大小，供背压测试精确跨阈值）。
fn fake_update(seed: u8, payload: usize) -> Vec<u8> {
    use yrs::{Map, ReadTxn, Transact, WriteTxn};
    let doc = yrs::Doc::new();
    let mut txn = doc.transact_mut();
    let text = format!("{seed:04}-{}", "x".repeat(payload));
    txn.get_or_insert_map("root").insert(&mut txn, "k", text);
    drop(txn);
    let txn = doc.transact();
    txn.encode_state_as_update_v1(&yrs::StateVector::default())
}

#[tokio::test]
async fn backpressure_decision_table() {
    let soft = BACKPRESSURE_SOFT_BYTES;
    let hard = BACKPRESSURE_HARD_BYTES;
    assert_eq!(
        decide_backpressure(soft, soft, hard),
        BackpressureAction::FlushDirect
    );
    assert_eq!(
        decide_backpressure(soft + 1, soft, hard),
        BackpressureAction::Merge
    );
    assert_eq!(
        decide_backpressure(hard, soft, hard),
        BackpressureAction::Merge
    );
    assert_eq!(
        decide_backpressure(hard + 1, soft, hard),
        BackpressureAction::Close
    );
}

#[tokio::test]
async fn fan_out_respects_subscriptions() {
    let b = Broadcaster::new(1_000_000, 2_000_000);
    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);
    let doc_a = DocId::chat("s1");
    let doc_b = DocId::chat("s2");

    b.subscribe(1, vec![doc_a.clone()], tx1).await.unwrap();
    b.subscribe(2, vec![doc_b.clone()], tx2).await.unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let handle = b.attach(in_rx);

    // 只订阅 doc_a 的连接 1 收到；连接 2 不收到。
    in_tx
        .send(DocUpdate {
            doc: doc_a.clone(),
            update: fake_update(1, 16),
        })
        .unwrap();

    let msg1 = tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv())
        .await
        .expect("conn1 should receive")
        .expect("conn1 tx alive");
    assert!(matches!(msg1, OutboundMsg::Frame(Frame::YsyncUpdate(u)) if u.doc == doc_a));
    // 连接 2 无订阅 → 收不到。
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv())
            .await
            .is_err()
    );

    // 退订后不再收。
    b.unsubscribe(1, vec![doc_a.clone()]).await;
    in_tx
        .send(DocUpdate {
            doc: doc_a,
            update: fake_update(2, 16),
        })
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv())
            .await
            .is_err()
    );

    handle.abort();
    let _ = rx2;
    let _ = json!(null);
}

#[tokio::test]
async fn soft_threshold_merges_updates() {
    // 慢消费者（§8.6）：发送队列容量 4，不消费 → 直发 4 帧填满队列后
    // try_send 失败，帧保留在 pending 缓冲；soft = 1KB（约 2 帧）→ 后续
    // 帧触发合并（`merge_updates_v1`），pending 以**合并帧**形式累积。
    let b = Broadcaster::new(1024, 64 * 1024);
    let (tx, mut rx) = mpsc::channel(4);
    let doc = DocId::chat("s1");
    b.subscribe(1, vec![doc.clone()], tx).await.unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let handle = b.attach(in_rx);

    for i in 0..8u8 {
        in_tx
            .send(DocUpdate {
                doc: doc.clone(),
                update: fake_update(i, 500),
            })
            .unwrap();
    }
    // 等 fan_out 处理完（非阻塞 try_send 路径，毫秒级完成）。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // 4 个直发帧已投递（填满容量 4 的队列）。
    let mut frames = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(OutboundMsg::Frame(Frame::YsyncUpdate(u))) => frames.push(u),
            Ok(other) => panic!("unexpected msg: {other:?}"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(e) => panic!("recv error: {e:?}"),
        }
    }
    assert_eq!(frames.len(), 4, "4 direct frames, got {}", frames.len());

    // 剩余 4 帧（f5-f8）在 pending 缓冲中合并为 1 帧（§8.6 merge_updates）。
    let subs = b.subs.read().await;
    let sub = subs.get(&1).expect("subscription alive");
    assert_eq!(sub.pending.len(), 1, "4 frames merged into 1 pending frame");
    let pending = sub.pending.front().unwrap();
    assert!(
        pending.1.len() > 2000,
        "merged pending update should hold ~4 frames ({} bytes)",
        pending.1.len()
    );
    use yrs::updates::decoder::Decode as _;
    yrs::Update::decode_v1(&pending.1).expect("merged pending update decodable");
    drop(subs);
    handle.abort();
}

#[tokio::test]
async fn slow_consumer_preserves_frame_order() {
    // 慢消费者（发送队列容量 1）下，保留回 pending 的帧必须**保持到达顺序**
    // （§8.6 队列语义；yjs 增量虽乱序可收敛，但序保持是承诺）。回归：曾以
    // push_front 逆序回填（[f2,f3] → [f3,f2]）。
    let b = Broadcaster::new(1_000_000, 2_000_000); // soft 足够大：不触发合并
    let (tx, rx) = mpsc::channel(1);
    let doc = DocId::chat("s1");
    b.subscribe(1, vec![doc.clone()], tx).await.unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let handle = b.attach(in_rx);

    let u2 = fake_update(2, 200);
    let u3 = fake_update(3, 200);
    // f1 填满队列（发出）；f2 因队列满保留；f3 到达不触发 flush（字节 < soft）
    // → 两帧均按到达顺序保留在 pending。
    in_tx
        .send(DocUpdate {
            doc: doc.clone(),
            update: fake_update(1, 200),
        })
        .unwrap();
    in_tx
        .send(DocUpdate {
            doc: doc.clone(),
            update: u2.clone(),
        })
        .unwrap();
    in_tx
        .send(DocUpdate {
            doc: doc.clone(),
            update: u3.clone(),
        })
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let subs = b.subs.read().await;
    let sub = subs.get(&1).expect("subscription alive");
    let frames: Vec<Vec<u8>> = sub.pending.iter().map(|(_, u)| u.clone()).collect();
    assert_eq!(frames, vec![u2, u3], "pending 帧序必须保持到达顺序");
    drop(subs);
    let _ = rx;
    handle.abort();
}

#[tokio::test]
async fn hard_threshold_closes_connection() {
    // 慢消费者 + soft = 1KB、hard = 1.5KB（约 3 帧）：直发 4 帧填满队列后
    // 帧 5 保留；帧 6 合并（~1060B ≤ hard）仍保留；帧 7 累计 > hard →
    // Close(1011) + 订阅移除（§8.6：以可恢复错误关闭连接）。
    let b = Broadcaster::new(1024, 1500);
    let (tx, mut rx) = mpsc::channel(4);
    let doc = DocId::chat("s1");
    b.subscribe(1, vec![doc.clone()], tx).await.unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let handle = b.attach(in_rx);

    for i in 0..7u8 {
        in_tx
            .send(DocUpdate {
                doc: doc.clone(),
                update: fake_update(i, 500),
            })
            .unwrap();
    }
    // 等 fan_out 处理完 + Close 信号经独立 task 投递（队列有空位后）。
    let mut closed = false;
    for _ in 0..16 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(OutboundMsg::Close(code))) => {
                assert_eq!(code, 1011, "backpressure close code (可恢复错误)");
                closed = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }
    assert!(closed, "connection should be closed at hard threshold");
    // 订阅已移除。
    assert_eq!(b.sub_count(1).await, 0);
    handle.abort();
}

#[tokio::test]
async fn unsubscribe_all_cleans() {
    let b = Broadcaster::new(1_000_000, 2_000_000);
    let (tx, _rx) = mpsc::channel(64);
    let doc = DocId::chat("s1");
    b.subscribe(1, vec![doc.clone()], tx).await.unwrap();
    assert_eq!(b.sub_count(1).await, 1);
    b.unsubscribe_all(1).await;
    assert_eq!(b.sub_count(1).await, 0);
    b.unsubscribe_all(1).await; // 幂等
}
