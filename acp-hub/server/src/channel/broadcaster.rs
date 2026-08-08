//! 状态广播（架构 §4.2 `ysync.update` S→C 单向，§8.6；设计稿
//! `f5-channel-control.md` §9）。
//!
//! 每连接每 doc 订阅（`ysync.subscribe` 驱动）。输入为 StoreSink（F5 装配层）
//! 的镜像更新流——**单一真相**：快照与增量同源同 clientID，客户端应用无
//! CRDT 分叉（设计稿 §9 输入源为 DocManager 广播；F5 装配改接镜像流，理由
//! 见 `hub.rs` StoreSink 文档——DocManager 无启动重放，镜像承担视图真相）。
//!
//! 背压（§8.6）：队列字节 ≤ soft（64KB）直接发；soft < 队列 ≤ hard（128KB）
//! 合并（`merge_updates_v1`）后单帧发送；> hard 以可恢复错误（1011）关闭
//! 连接（客户端重连后快照重同步兜底，§8.1 原则 4）。广播失败只影响连接
//! 传递，**不阻塞**更新产生侧（输入为独立 task）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use base64::Engine as _;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use acp_hub_proto::conn::DocId;
use acp_hub_proto::frame::Frame;
use acp_hub_proto::ysync::YsyncUpdate;

use crate::state::doc_manager::DocUpdate;
use crate::state::view_store::merge_updates_v1;

/// 背压软阈值（§16/§8.6 默认 64KB；装配默认值事实源，测试直接引用）。
#[allow(dead_code)] // 装配侧从 Config 取实际值；常量保留为 §16 事实源
pub const BACKPRESSURE_SOFT_BYTES: usize = 64 * 1024;
/// 背压硬阈值（§16/§8.6 默认 128KB；装配默认值事实源，测试直接引用）。
#[allow(dead_code)]
pub const BACKPRESSURE_HARD_BYTES: usize = 128 * 1024;

/// 订阅错误（连接侧队列失效/断开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubError {
    /// 发送队列已关闭（连接正在关闭）。
    #[error("connection send queue closed")]
    Closed,
}

/// 出站消息（gateway 连接发送队列的统一载荷）。
///
/// broadcaster / coordinator / gateway / machine registry 都向连接发送队列
/// 投递；`Close` 携带关闭码（§4.7），gateway 消费后以对应码关闭 ws。
#[derive(Debug, Clone)]
pub enum OutboundMsg {
    /// 业务帧（action_ack/action_error/ready/keep_alive/ysync.update/...）。
    Frame(Frame),
    /// JSON-RPC 透传（machine 面出站：prompt/cancel/resolve/initialize/
    /// session/new，§4.5 透传语义——machine 保持 dumb）。
    JsonRpc(serde_json::Value),
    /// 连接关闭信号（背压硬阈值 1011 / 机器离线 4500 等）。
    Close(u16),
}

/// 背压裁决（纯函数，供测试与实现共用，§8.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressureAction {
    /// 逐帧直接发送（队列 ≤ soft）。
    FlushDirect,
    /// 合并为单帧发送（soft < 队列 ≤ hard）。
    Merge,
    /// 关闭连接（> hard，可恢复错误 1011）。
    Close,
}

/// 按累计字节数裁决（§8.6 阈值）。
pub fn decide_backpressure(pending_bytes: usize, soft: usize, hard: usize) -> BackpressureAction {
    if pending_bytes > hard {
        BackpressureAction::Close
    } else if pending_bytes > soft {
        BackpressureAction::Merge
    } else {
        BackpressureAction::FlushDirect
    }
}

/// 单连接订阅（docs 集 + 发送队列 + 背压缓冲）。
#[derive(Debug)]
struct ConnSub {
    docs: HashSet<DocId>,
    tx: mpsc::Sender<OutboundMsg>,
    /// 待 flush 缓冲（合并路径）。
    pending: VecDeque<(DocId, Vec<u8>)>,
    bytes: usize,
}

/// 状态广播器（§8.6 fan-out）。
#[derive(Clone, Debug)]
pub struct Broadcaster {
    soft: usize,
    hard: usize,
    subs: Arc<RwLock<HashMap<u64, ConnSub>>>,
}

impl Broadcaster {
    /// 以背压阈值构建（§16 默认 64KB/128KB）。
    pub fn new(soft: usize, hard: usize) -> Self {
        Broadcaster {
            soft,
            hard,
            subs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 附着更新流（hub 装配时调用一次；返回后台 fan-out task 句柄）。
    ///
    /// 输入为 StoreSink 镜像更新流（见模块文档）；task 生命周期随调用方
    /// `JoinHandle` 管理（优雅关闭时 abort）。
    pub fn attach(&self, mut rx: mpsc::UnboundedReceiver<DocUpdate>) -> JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                this.fan_out(update).await;
            }
            debug!("broadcaster input stream closed");
        })
    }

    /// 客户端订阅（`ysync.subscribe` 驱动；tx 为 gateway 的 ws 发送队列）。
    /// 重复订阅同一 doc 幂等。
    pub async fn subscribe(
        &self,
        conn_id: u64,
        docs: Vec<DocId>,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> Result<(), SubError> {
        let mut subs = self.subs.write().await;
        let sub = subs.entry(conn_id).or_insert_with(|| ConnSub {
            docs: HashSet::new(),
            tx,
            pending: VecDeque::new(),
            bytes: 0,
        });
        for d in docs {
            sub.docs.insert(d);
        }
        Ok(())
    }

    /// 退订（`ysync.unsubscribe` 驱动；幂等）。
    pub async fn unsubscribe(&self, conn_id: u64, docs: Vec<DocId>) {
        let mut subs = self.subs.write().await;
        if let Some(sub) = subs.get_mut(&conn_id) {
            for d in docs {
                sub.docs.remove(&d);
            }
        }
    }

    /// 连接断开清理（gateway 调用）。
    pub async fn unsubscribe_all(&self, conn_id: u64) {
        self.subs.write().await.remove(&conn_id);
    }

    /// 某连接订阅的 doc 数（诊断）。
    pub async fn sub_count(&self, conn_id: u64) -> usize {
        self.subs
            .read()
            .await
            .get(&conn_id)
            .map(|s| s.docs.len())
            .unwrap_or(0)
    }

    /// fan-out（attach task 内）：按 doc 路由 + 背压裁决。
    async fn fan_out(&self, update: DocUpdate) {
        let conns: Vec<u64> = {
            let subs = self.subs.read().await;
            subs.iter()
                .filter(|(_, s)| s.docs.contains(&update.doc))
                .map(|(id, _)| *id)
                .collect()
        };
        for conn_id in conns {
            self.deliver(conn_id, update.doc.clone(), update.update.clone())
                .await;
        }
    }

    /// 单连接投递（§8.6 背压语义的载体）。
    ///
    /// 帧入缓冲后判定是否 flush：【决策】`pending.len() == 1`（无积压的
    /// 单帧）立即 flush 保实时；多帧积压则按**累计字节 ≥ soft** 才 flush——
    /// 「队列」= pending 缓冲字节数，裁决 `decide_backpressure` 在 flush 前
    /// 执行：≤ soft 逐帧直发；soft < ≤ hard 合并（`merge_updates_v1`）为
    /// 单帧；> hard 关闭连接（1011）。慢消费者（发送队列满）下 try_send
    /// 失败 → 帧**保留在 pending 继续累积**（等效「跳过发送」，§8.6；
    /// 客户端重连后快照重同步兜底，§8.1 原则 4）。
    async fn deliver(&self, conn_id: u64, doc: DocId, update: Vec<u8>) {
        let action = {
            let mut subs = self.subs.write().await;
            let Some(sub) = subs.get_mut(&conn_id) else {
                return;
            };
            sub.pending.push_back((doc, update));
            sub.bytes += sub.pending.back().expect("just pushed").1.len();
            // 单帧立即 flush；积压帧按字节阈值 flush（§8.6 队列语义）。
            let flush = sub.pending.len() == 1 || sub.bytes >= self.soft;
            flush.then(|| decide_backpressure(sub.bytes, self.soft, self.hard))
        };
        match action {
            Some(BackpressureAction::Close) => {
                let bytes = self.bytes_of(conn_id).await;
                warn!(
                    conn_id, bytes,
                    "broadcast backpressure hard threshold exceeded; closing connection"
                );
                self.close_conn(conn_id).await;
            }
            Some(BackpressureAction::Merge) => {
                self.flush_merged(conn_id).await;
            }
            Some(BackpressureAction::FlushDirect) => {
                self.flush_direct(conn_id).await;
            }
            None => {
                // 积压中，等待下一帧或字节阈值触发 flush。
                debug!(conn_id, "broadcast buffered (backpressure)");
            }
        }
    }

    async fn bytes_of(&self, conn_id: u64) -> usize {
        self.subs
            .read()
            .await
            .get(&conn_id)
            .map(|s| s.bytes)
            .unwrap_or(0)
    }

    /// 直接逐帧发送（≤ soft）。非阻塞投递：发送队列满（慢消费者）时帧**保留
    /// 在 pending 缓冲**继续累积（§8.6 背压语义——pending 是「队列」的载体，
    /// 累积到 soft 以上触发合并、hard 以上关闭连接）；队列关闭 → 移除订阅。
    async fn flush_direct(&self, conn_id: u64) {
        let mut subs = self.subs.write().await;
        let Some(sub) = subs.get_mut(&conn_id) else {
            return;
        };
        let tx = sub.tx.clone();
        let mut pending: VecDeque<(DocId, Vec<u8>)> = std::mem::take(&mut sub.pending);
        let mut kept: VecDeque<(DocId, Vec<u8>)> = VecDeque::new();
        let mut kept_bytes = 0usize;
        while let Some((doc, update)) = pending.pop_front() {
            match tx.try_send(OutboundMsg::Frame(encode_update_frame(doc.clone(), update.clone()))) {
                Ok(()) => continue,
                Err(TrySendError::Full(_)) => {
                    // 慢消费者：本帧与剩余帧全部保留（保持序，push_back 不颠倒
                    // 帧序——yjs 增量乱序应用虽收敛，但序保持是 §8.6 队列语义
                    // 的承诺），背压由 pending 累积承载——下一帧到达时按累计
                    // 字节裁决（§8.6）。
                    kept_bytes += update.len();
                    kept.push_back((doc, update));
                    while let Some((d, u)) = pending.pop_front() {
                        kept_bytes += u.len();
                        kept.push_back((d, u));
                    }
                    break;
                }
                Err(TrySendError::Closed(_)) => {
                    break;
                }
            }
        }
        sub.pending = kept;
        sub.bytes = kept_bytes;
        if sub.tx.is_closed() {
            let _ = subs.remove(&conn_id);
        }
    }

    /// 合并后单帧发送（soft < 队列 ≤ hard；`merge_updates_v1` 按 doc 分组
    /// 合并；合并失败（编码异常）→ 逐帧回落；发送队列满 → 合并帧保留在
    /// pending 继续累积，直至 hard 阈值关闭连接，§8.6）。
    async fn flush_merged(&self, conn_id: u64) {
        let mut subs = self.subs.write().await;
        let Some(sub) = subs.get_mut(&conn_id) else {
            return;
        };
        // 按 doc 分组合并（§6.4 merge_updates 语义）。
        let mut by_doc: HashMap<DocId, Vec<Vec<u8>>> = HashMap::new();
        for (doc, update) in sub.pending.drain(..) {
            by_doc.entry(doc).or_default().push(update);
        }
        sub.bytes = 0;
        let tx = sub.tx.clone();
        let frames: Vec<OutboundMsg> = by_doc
            .into_iter()
            .map(|(doc, updates)| {
                match merge_updates_v1(&updates) {
                    Ok(merged) => OutboundMsg::Frame(encode_update_frame(doc, merged)),
                    Err(e) => {
                        warn!(conn_id, error = ?e, "update merge failed; falling back to first frame");
                        OutboundMsg::Frame(encode_update_frame(doc, updates[0].clone()))
                    }
                }
            })
            .collect();
        // 非阻塞投递：满 → 本帧与剩余帧保留回 pending（后续裁决继续累积）。
        let mut kept: VecDeque<(DocId, Vec<u8>)> = VecDeque::new();
        let mut kept_bytes = 0usize;
        let mut frames = frames.into_iter();
        while let Some(f) = frames.next() {
            let (doc, update) = match &f {
                OutboundMsg::Frame(Frame::YsyncUpdate(u)) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&u.update)
                        .unwrap_or_default();
                    (u.doc.clone(), bytes)
                }
                _ => unreachable!("only update frames here"),
            };
            match tx.try_send(f) {
                Ok(()) => continue,
                Err(TrySendError::Full(_)) => {
                    kept_bytes += update.len();
                    kept.push_back((doc, update));
                    for rest in frames {
                        let (d, u) = match &rest {
                            OutboundMsg::Frame(Frame::YsyncUpdate(u2)) => {
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(&u2.update)
                                    .unwrap_or_default();
                                (u2.doc.clone(), bytes)
                            }
                            _ => unreachable!("only update frames here"),
                        };
                        kept_bytes += u.len();
                        kept.push_back((d, u));
                    }
                    break;
                }
                Err(TrySendError::Closed(_)) => break,
            }
        }
        sub.pending = kept;
        sub.bytes = kept_bytes;
        if sub.tx.is_closed() {
            let _ = subs.remove(&conn_id);
        }
        debug!(conn_id, "broadcast merged and flushed");
    }

    /// 硬阈值超限：向连接发送关闭信号（1011 通用失败，退避重连，§4.7）并
    /// 移除订阅（gateway 收到 `OutboundMsg::Close` 后关闭 ws）。
    ///
    /// 关闭信号经独立 task 阻塞式投递：发送队列可能正满（慢消费者），
    /// await 等待空位不阻塞 fan-out 路径（§8.1 原则 4）。
    async fn close_conn(&self, conn_id: u64) {
        let tx = {
            let mut subs = self.subs.write().await;
            let Some(sub) = subs.remove(&conn_id) else {
                return;
            };
            sub.tx.clone()
        };
        tokio::spawn(async move {
            let _ = tx.send(OutboundMsg::Close(1011)).await;
        });
    }
}

/// y-sync 编码：update bytes → base64 帧（§4.1；快照带 projection_version，
/// 增量不带——broadcaster 只发增量，`None` 序列化跳过）。
fn encode_update_frame(doc: DocId, update: Vec<u8>) -> Frame {
    Frame::YsyncUpdate(YsyncUpdate {
        doc,
        update: base64::engine::general_purpose::STANDARD.encode(&update),
        projection_version: None,
    })
}

#[cfg(test)]
#[path = "broadcaster_test.rs"]
mod broadcaster_test;
