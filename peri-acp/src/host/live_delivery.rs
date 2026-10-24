//! History V2 实时投递：每连接、每会话一条有序 `session/update` 流（设计 §6.3、§9.5）。
//!
//! 连接在 V2 `session/load` 装配首屏前建立（或替换）本会话的流并暂停：首屏装配期间
//! 产生的 transcript 更新进入有界缓冲，**load 响应发送之后**才按序补发；之后进入
//! 直发状态。每条 transcript 更新在顶层 `_meta["peri.delivery"]` 上携带
//! `streamId` / `deliveryId` / `eventSeq`（十进制字符串），客户端据此检测缺口。
//!
//! 注册表归连接所有（[`super::connection::ConnectionContext`]），不跨连接共享；会话侧
//! 只持有 `Arc` 供事件 sink 选用。`close` 即移除流：之后的更新原样透传，不挂投递元数据。
//!
//! 锁是 `std::sync::Mutex`：`publish` 在事件发送路径上同步执行，临界区内只做内存操作，
//! 不等待网络；发送（`send_notification`）始终在锁外完成。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use peri_acp_types::history_v2::{DecimalU64, HistoryLiveBoundary};
use serde_json::{json, Value};

/// transcript 通知上的投递元数据命名空间。
pub(crate) const DELIVERY_META_KEY: &str = "peri.delivery";

/// 暂停期间最多保留的更新条数（与设计 §10.5 客户端首屏暂存分开：服务端更小）。
pub(crate) const MAX_BUFFERED_UPDATES: usize = 256;

/// 暂停期间最多保留的序列化字节数。
pub(crate) const MAX_BUFFERED_BYTES: usize = 1024 * 1024;

/// 一条待补发的 transcript 更新。
struct BufferedUpdate {
    payload: Value,
    bytes: usize,
}

struct LiveStream {
    stream_id: String,
    /// 已发出的最后序号；暂停期间不推进，恢复时按缓冲顺序续号。
    next_seq: u64,
    paused: bool,
    buffer: VecDeque<BufferedUpdate>,
    buffered_bytes: usize,
    /// 缓冲溢出丢弃的条数：恢复时先推进序号，让客户端看到缺口而不是伪装连续。
    dropped: u64,
}

impl LiveStream {
    fn new(stream_id: String, next_seq: u64) -> Self {
        Self {
            stream_id,
            next_seq,
            paused: true,
            buffer: VecDeque::new(),
            buffered_bytes: 0,
            dropped: 0,
        }
    }

    /// 缓冲一条更新；超过条数或字节上限时丢最旧并记入缺口。
    fn buffer(&mut self, payload: Value) -> PublishOutcome {
        let bytes = serde_json::to_vec(&payload)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        self.buffer.push_back(BufferedUpdate { payload, bytes });
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
        let mut overflowed = false;
        while self.buffer.len() > MAX_BUFFERED_UPDATES || self.buffered_bytes > MAX_BUFFERED_BYTES {
            let Some(dropped) = self.buffer.pop_front() else {
                break;
            };
            self.buffered_bytes = self.buffered_bytes.saturating_sub(dropped.bytes);
            self.dropped = self.dropped.saturating_add(1);
            overflowed = true;
        }
        if overflowed {
            PublishOutcome::Overflow
        } else {
            PublishOutcome::Buffered
        }
    }

    /// 给一条更新分配序号并写入投递元数据。
    fn deliver(&mut self, mut payload: Value) -> Value {
        self.next_seq = self.next_seq.saturating_add(1);
        attach_delivery(&mut payload, &self.stream_id, self.next_seq);
        payload
    }
}

/// 每个连接一份的实时投递注册表。
pub(crate) struct LiveDelivery {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    streams: HashMap<String, LiveStream>,
    /// 尚未提交的 initial 候选流（每会话至多一个）。
    ///
    /// 候选从旧流的 `next_seq` 起步并独立缓冲；提交前旧流保持原样（FIX-05）。
    candidates: HashMap<String, CandidateStream>,
}

struct CandidateStream {
    candidate_id: String,
    stream: LiveStream,
}

/// initial 准备阶段建立、尚未提交的候选流。
///
/// `boundary` 是提交成功后客户端应使用的 `liveBoundary`；候选未提交前旧流仍然有效。
pub(crate) struct LiveCandidate {
    pub(crate) candidate_id: String,
    pub(crate) boundary: HistoryLiveBoundary,
}

/// `publish` 的结果：直发、已缓冲，或因溢出丢弃了最旧更新。
#[derive(Debug)]
pub(crate) enum PublishOutcome {
    /// 调用方应立即发送该 payload（stream 不存在时也是原样 payload）。
    Send(Value),
    /// 流处于暂停期，更新已进入有界缓冲，不发送。
    Buffered,
    /// 已丢弃最旧的缓冲更新；调用方记录后继续，客户端会因序号缺口进入 detached。
    Overflow,
}

impl Default for LiveDelivery {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveDelivery {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 锁内只做内存操作，毒化守卫按内部不变式恢复即可，不把 panic 传播到发送路径。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 建立本会话的新流（替换同会话旧流），返回首屏交付边界。
    ///
    /// `afterSeq` 沿用旧流已发出的最后序号：重复 initial 时客户端已经接收过这些事件，
    /// 新首屏快照与它们一致；新流的 `eventSeq` 从 `afterSeq + 1` 继续。没有旧流时从 0 开始。
    ///
    /// 注意：生产 initial 使用 [`Self::begin_candidate`] + [`Self::commit_candidate`]；
    /// 本方法只在旧流已经可以丢弃时使用，当前仅测试直接建立暂停流。
    #[cfg(test)]
    pub(crate) fn begin(&self, session_id: &str) -> HistoryLiveBoundary {
        let mut inner = self.lock();
        let after_seq = inner
            .streams
            .get(session_id)
            .map(|stream| stream.next_seq)
            .unwrap_or(0);
        let stream_id = format!("live:{}", uuid::Uuid::now_v7());
        inner.streams.insert(
            session_id.to_owned(),
            LiveStream::new(stream_id.clone(), after_seq),
        );
        HistoryLiveBoundary {
            stream_id,
            after_seq: DecimalU64::new(after_seq),
        }
    }

    /// 建立候选流：旧流（streamId / 序号 / 缓冲）保持不变，候选独立缓冲实时更新。
    ///
    /// 候选从旧流当前 `next_seq` 起步，提交后事件序号连续；提交前旧流仍按原状态投递。
    /// 同会话已有候选时替换之：旧候选的 guard 会因 `candidateId` 不匹配而无法提交。
    pub(crate) fn begin_candidate(&self, session_id: &str) -> LiveCandidate {
        let mut inner = self.lock();
        let after_seq = inner
            .streams
            .get(session_id)
            .map(|stream| stream.next_seq)
            .unwrap_or(0);
        let stream_id = format!("live:{}", uuid::Uuid::now_v7());
        let candidate_id = format!("cand_{}", uuid::Uuid::new_v4().simple());
        inner.candidates.insert(
            session_id.to_owned(),
            CandidateStream {
                candidate_id: candidate_id.clone(),
                stream: LiveStream::new(stream_id.clone(), after_seq),
            },
        );
        LiveCandidate {
            candidate_id,
            boundary: HistoryLiveBoundary {
                stream_id,
                after_seq: DecimalU64::new(after_seq),
            },
        }
    }

    /// 提交候选：用候选流替换旧流。候选已被回收/替换时返回 false，旧流不受影响。
    pub(crate) fn commit_candidate(&self, session_id: &str, candidate_id: &str) -> bool {
        let mut inner = self.lock();
        let matches = inner
            .candidates
            .get(session_id)
            .is_some_and(|candidate| candidate.candidate_id == candidate_id);
        if !matches {
            return false;
        }
        let Some(candidate) = inner.candidates.remove(session_id) else {
            return false;
        };
        inner
            .streams
            .insert(session_id.to_owned(), candidate.stream);
        true
    }

    /// 回收候选；旧流保持不变。用于失败、取消、超时与 drop。
    pub(crate) fn abort_candidate(&self, session_id: &str, candidate_id: &str) -> bool {
        let mut inner = self.lock();
        let matches = inner
            .candidates
            .get(session_id)
            .is_some_and(|candidate| candidate.candidate_id == candidate_id);
        if matches {
            inner.candidates.remove(session_id);
        }
        matches
    }

    /// 发布一条 transcript 更新。
    ///
    /// - 流不存在（非 V2 会话或已 close）：原样返回，不挂投递元数据；
    /// - 暂停期：进入有界缓冲；
    /// - 活动期：分配序号、写入元数据后返回待发送 payload。
    pub(crate) fn publish(&self, session_id: &str, payload: Value) -> PublishOutcome {
        let mut inner = self.lock();
        // initial 准备期间的新事件进入候选缓冲：旧流身份、序号与缓冲保持不变。
        if let Some(candidate) = inner.candidates.get_mut(session_id) {
            return candidate.stream.buffer(payload);
        }
        let Some(stream) = inner.streams.get_mut(session_id) else {
            return PublishOutcome::Send(payload);
        };
        if stream.paused {
            return stream.buffer(payload);
        }
        PublishOutcome::Send(stream.deliver(payload))
    }

    /// 结束暂停：按序为缓冲更新分配序号并返回待发送列表。
    ///
    /// 溢出丢弃的条数先推进 `next_seq`，因此第一条待发送事件的序号大于 `afterSeq + 1`，
    /// 客户端据此检测缺口。调用后流进入直发状态；没有流时返回空列表。
    pub(crate) fn resume_and_take(&self, session_id: &str) -> Vec<Value> {
        let mut inner = self.lock();
        let Some(stream) = inner.streams.get_mut(session_id) else {
            return Vec::new();
        };
        stream.paused = false;
        if stream.dropped > 0 {
            stream.next_seq = stream.next_seq.saturating_add(stream.dropped);
            stream.dropped = 0;
        }
        let buffered = std::mem::take(&mut stream.buffer);
        stream.buffered_bytes = 0;
        buffered
            .into_iter()
            .map(|update| stream.deliver(update.payload))
            .collect()
    }

    /// 关闭并移除本会话的流与未提交候选；缓冲一并丢弃。之后的更新原样透传。
    pub(crate) fn close(&self, session_id: &str) {
        let mut inner = self.lock();
        inner.streams.remove(session_id);
        inner.candidates.remove(session_id);
    }

    /// 只关闭仍处于暂停期的流（load 失败/响应未送达的清理路径）。
    ///
    /// 对已进入直发状态的流是 no-op：一次失败的 initial 重试不得拆除既有实时交付。
    pub(crate) fn close_if_paused(&self, session_id: &str) {
        let mut inner = self.lock();
        if inner
            .streams
            .get(session_id)
            .is_some_and(|stream| stream.paused)
        {
            inner.streams.remove(session_id);
        }
    }

    /// 连接关闭：丢弃该连接的全部流、候选与缓冲（SessionState 可能仍持有 `Arc`）。
    pub(crate) fn close_all(&self) {
        let mut inner = self.lock();
        inner.streams.clear();
        inner.candidates.clear();
    }

    /// 重放校验：会话当前是否仍由 `stream_id` 标识的流承载。
    pub(crate) fn has_stream_with_id(&self, session_id: &str, stream_id: &str) -> bool {
        self.lock()
            .streams
            .get(session_id)
            .is_some_and(|stream| stream.stream_id == stream_id)
    }

    #[cfg(test)]
    pub(crate) fn has_stream(&self, session_id: &str) -> bool {
        self.lock().streams.contains_key(session_id)
    }

    #[cfg(test)]
    pub(crate) fn is_paused(&self, session_id: &str) -> bool {
        self.lock()
            .streams
            .get(session_id)
            .is_some_and(|stream| stream.paused)
    }

    #[cfg(test)]
    pub(crate) fn buffered_len(&self, session_id: &str) -> usize {
        self.lock()
            .streams
            .get(session_id)
            .map(|stream| stream.buffer.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn buffered_bytes(&self, session_id: &str) -> usize {
        self.lock()
            .streams
            .get(session_id)
            .map(|stream| stream.buffered_bytes)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn has_candidate(&self, session_id: &str) -> bool {
        self.lock().candidates.contains_key(session_id)
    }

    #[cfg(test)]
    pub(crate) fn candidate_buffered_len(&self, session_id: &str) -> usize {
        self.lock()
            .candidates
            .get(session_id)
            .map(|candidate| candidate.stream.buffer.len())
            .unwrap_or(0)
    }
}

/// 在 payload 顶层 `_meta` 写入投递身份，保留既有 `_meta` 键（如 sourceAgentId）。
fn attach_delivery(payload: &mut Value, stream_id: &str, seq: u64) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    let meta = object
        .entry("_meta")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !meta.is_object() {
        *meta = Value::Object(serde_json::Map::new());
    }
    let Some(meta) = meta.as_object_mut() else {
        return;
    };
    meta.insert(
        DELIVERY_META_KEY.to_owned(),
        json!({
            "streamId": stream_id,
            "deliveryId": format!("{stream_id}:{seq}"),
            "eventSeq": seq.to_string(),
        }),
    );
}

/// 非 V2 路径接管会话时撤销实时绑定。
///
/// 标准 `session/load`/`session/resume` 的连接不得继承上一条连接的投递身份：旧流关闭，
/// 会话侧的 `Arc` 清空，后续事件保持 V1 wire 形状。
pub(crate) async fn detach_session(sessions: &super::SharedSessions, session_id: &str) {
    let previous = {
        let mut states = sessions.lock().await;
        take_live_delivery(&mut states, session_id)
    };
    if let Some(previous) = previous {
        previous.close(session_id);
    }
}

/// [`detach_session`] 的同步形式，供持有 `&mut HashMap` 的进程内 load 入口使用。
pub(crate) fn detach_session_state(
    sessions: &mut std::collections::HashMap<String, super::SessionState>,
    session_id: &str,
) {
    if let Some(previous) = take_live_delivery(sessions, session_id) {
        previous.close(session_id);
    }
}

fn take_live_delivery(
    sessions: &mut std::collections::HashMap<String, super::SessionState>,
    session_id: &str,
) -> Option<std::sync::Arc<LiveDelivery>> {
    sessions
        .get_mut(session_id)
        .and_then(|state| state.live_delivery.take())
}

#[cfg(test)]
#[path = "live_delivery_test.rs"]
mod tests;
