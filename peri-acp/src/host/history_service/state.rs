//! 连接级窗口注册表与单调时钟。
//!
//! 注册表只保存轻量事实（快照身份、票据、home cursor、期限），不保存任何页面内容。
//! 所有方法都是同步短操作；调用方在持有 `ServiceState` 锁时**不得** await 后端。

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use peri_acp_types::history_v2::HistorySnapshotRef;
use tokio_util::sync::CancellationToken;

/// 窗口空闲期限：最后一次成功校验后 5 分钟。
pub(super) const WINDOW_IDLE_TTL: Duration = Duration::from_secs(5 * 60);
/// 窗口绝对期限：创建后 30 分钟。
pub(super) const WINDOW_ABSOLUTE_TTL: Duration = Duration::from_secs(30 * 60);
/// `Opening` 占位最长时间；超时清理，防止取消泄漏。
pub(super) const OPENING_TTL: Duration = Duration::from_secs(60);
/// 已关闭窗口身份保留时间。
pub(super) const CLOSED_TTL: Duration = Duration::from_secs(30 * 60);
/// 已关闭窗口身份上限。
pub(super) const MAX_CLOSED_WINDOWS: usize = 128;
/// 单窗口已签发 bodyRef 上限；页交付的 opaque 条目数量本身受 `maxItems`（≤100）约束。
pub(super) const MAX_ISSUED_BODY_REFS: usize = 100;

/// 一个窗口已交付给客户端的 opaque `bodyRef`。
///
/// body 不是 MAC cursor，无法自证身份；服务端只接受本窗口实际交付过的引用，
/// 伪造/复用未交付条目的 ref 一律 `not_found`。集合随窗口移除整体丢弃。
#[derive(Default)]
struct IssuedBodyRefs {
    refs: HashSet<String>,
    order: VecDeque<String>,
}

impl IssuedBodyRefs {
    fn insert_all(&mut self, body_refs: impl IntoIterator<Item = String>) {
        for body_ref in body_refs {
            if !self.refs.insert(body_ref.clone()) {
                continue;
            }
            self.order.push_back(body_ref);
        }
        // 交付量远超窗口保留预算时按交付顺序淘汰最旧引用；被淘汰的 ref 与
        // `window_expired` 一样需要重新 open，不会静默续读。
        while self.order.len() > MAX_ISSUED_BODY_REFS {
            if let Some(oldest) = self.order.pop_front() {
                self.refs.remove(&oldest);
            }
        }
    }

    fn contains(&self, body_ref: &str) -> bool {
        self.refs.contains(body_ref)
    }
}

/// 服务内单调时钟；测试用 `ManualClock` 注入。
pub(crate) trait HistoryClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// 生产时钟：以创建时刻为原点。
pub(crate) struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl HistoryClock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// 窗口在途读的取消句柄。
///
/// `closed` 区分「显式关闭/会话拆除」与「过期/取消清理」：两者都撤销后端读取，
/// 但迟到结果分别映射为 `window_closed` 与 `window_expired`。
#[derive(Clone)]
pub(super) struct WindowCancel {
    token: CancellationToken,
    closed: Arc<AtomicBool>,
}

impl WindowCancel {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancel(&self, closed: bool) {
        if closed {
            self.closed.store(true, Ordering::SeqCst);
        }
        self.token.cancel();
    }

    pub(super) async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    pub(super) fn was_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// 窗口记录；`Opening` 阶段没有快照（后端尚未返回）。
pub(super) struct WindowRecord {
    pub(super) window_id: String,
    pub(super) snapshot_token: String,
    pub(super) read_ticket: String,
    pub(super) session_id: String,
    pub(super) snapshot: Option<HistorySnapshotRef>,
    pub(super) before_home: Option<String>,
    pub(super) after_home: Option<String>,
    pub(super) created_at: Duration,
    pub(super) last_used_at: Duration,
    pub(super) opening_started_at: Option<Duration>,
    /// 本窗口的在途读取消信号：close/session close/过期/连接拆除时触发。
    cancel: WindowCancel,
    /// 本窗口已交付的 opaque `bodyRef`；body 授权只看这里。
    issued_body_refs: IssuedBodyRefs,
}

impl WindowRecord {
    pub(super) fn opening(
        window_id: String,
        snapshot_token: String,
        read_ticket: String,
        session_id: String,
        now: Duration,
    ) -> Self {
        Self {
            window_id,
            snapshot_token,
            read_ticket,
            session_id,
            snapshot: None,
            before_home: None,
            after_home: None,
            created_at: now,
            last_used_at: now,
            opening_started_at: Some(now),
            cancel: WindowCancel::new(),
            issued_body_refs: IssuedBodyRefs::default(),
        }
    }

    pub(super) fn cancel_handle(&self) -> WindowCancel {
        self.cancel.clone()
    }

    fn is_expired(&self, now: Duration) -> bool {
        if let Some(started) = self.opening_started_at {
            return now.saturating_sub(started) >= OPENING_TTL;
        }
        now.saturating_sub(self.created_at) >= WINDOW_ABSOLUTE_TTL
            || now.saturating_sub(self.last_used_at) >= WINDOW_IDLE_TTL
    }
}

/// 通过校验后交给读路径的不可变窗口事实。
#[derive(Clone)]
pub(super) struct WindowAccess {
    pub(super) window_id: String,
    pub(super) snapshot_token: String,
    pub(super) read_ticket: String,
    pub(super) session_id: String,
    pub(super) snapshot: HistorySnapshotRef,
    pub(super) before_home: Option<String>,
    pub(super) after_home: Option<String>,
    /// 窗口关闭/移除时取消在途后端读取；迟到结果不得发布。
    pub(super) cancel: WindowCancel,
}

struct ClosedWindow {
    window_id: String,
    closed_at: Duration,
}

/// 连接级窗口注册表。
#[derive(Default)]
pub(super) struct WindowRegistry {
    windows: BTreeMap<String, WindowRecord>,
    closed: VecDeque<ClosedWindow>,
}

impl WindowRegistry {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// 清理过期窗口、`Opening` 占位与过龄的 closed 身份。
    pub(super) fn prune(&mut self, now: Duration) {
        let expired: Vec<String> = self
            .windows
            .iter()
            .filter(|(_, record)| record.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(record) = self.windows.remove(&id) {
                record.cancel.cancel(false);
            }
        }
        while self
            .closed
            .front()
            .is_some_and(|closed| now.saturating_sub(closed.closed_at) >= CLOSED_TTL)
        {
            self.closed.pop_front();
        }
        while self.closed.len() > MAX_CLOSED_WINDOWS {
            self.closed.pop_front();
        }
    }

    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.windows.len()
    }

    #[cfg(test)]
    pub(super) fn opening_count(&self) -> usize {
        self.windows
            .values()
            .filter(|record| record.snapshot.is_none())
            .count()
    }

    /// 是否仍是一个未过期窗口（用于幂等成功记录的有效性）。
    pub(super) fn is_live(&self, window_id: &str) -> bool {
        // 调用方在 `begin` 前已 prune，因此这里只需判断存在性。
        self.windows.contains_key(window_id)
    }

    /// 占位；容量按 `max` 限制。调用方先 `prune`。
    ///
    /// 替换预留（FIX-05）：同会话已有可用窗口且总数恰好达到 `max` 时，允许一个额外
    /// 候选槽（总数上限 `max + 1`），用于「先准备、后提交」的窗口替换。候选未提交前
    /// 旧窗口保持可读；提交或回收后计数回到 `max` 内。同一会话同时只允许一个
    /// `Opening` 占位，避免并发初始装载累积候选。
    pub(super) fn reserve(
        &mut self,
        record: WindowRecord,
        max: u32,
        allow_replacement: bool,
    ) -> Result<(), ()> {
        let same_session_opening = self.windows.values().any(|existing| {
            existing.session_id == record.session_id && existing.snapshot.is_none()
        });
        if same_session_opening {
            return Err(());
        }
        if self.windows.len() < max as usize {
            self.windows.insert(record.window_id.clone(), record);
            return Ok(());
        }
        let replaces_same_session = allow_replacement
            && self.windows.len() == max as usize
            && self
                .windows
                .values()
                .any(|existing| existing.session_id == record.session_id);
        if replaces_same_session {
            self.windows.insert(record.window_id.clone(), record);
            return Ok(());
        }
        Err(())
    }

    /// 把 `Opening` 升级为 `Ready`；占位已被清理时返回 false。
    pub(super) fn complete(
        &mut self,
        window_id: &str,
        snapshot: HistorySnapshotRef,
        before_home: Option<String>,
        after_home: Option<String>,
        now: Duration,
    ) -> bool {
        let Some(record) = self.windows.get_mut(window_id) else {
            return false;
        };
        if record.snapshot.is_some() {
            return false;
        }
        record.snapshot = Some(snapshot);
        record.before_home = before_home;
        record.after_home = after_home;
        record.opening_started_at = None;
        record.last_used_at = now;
        true
    }

    pub(super) fn get(&self, window_id: &str) -> Option<&WindowRecord> {
        self.windows.get(window_id)
    }

    pub(super) fn touch(&mut self, window_id: &str, now: Duration) {
        if let Some(record) = self.windows.get_mut(window_id) {
            record.last_used_at = now;
        }
    }

    pub(super) fn set_homes(
        &mut self,
        window_id: &str,
        before_home: Option<String>,
        after_home: Option<String>,
    ) {
        if let Some(record) = self.windows.get_mut(window_id) {
            record.before_home = before_home;
            record.after_home = after_home;
        }
    }

    /// 登记一页实际交付的 opaque `bodyRef`；窗口已不存在时返回 false。
    pub(super) fn issue_body_refs(
        &mut self,
        window_id: &str,
        body_refs: impl IntoIterator<Item = String>,
    ) -> bool {
        let Some(record) = self.windows.get_mut(window_id) else {
            return false;
        };
        record.issued_body_refs.insert_all(body_refs);
        true
    }

    /// 该窗口是否实际交付过这个 `bodyRef`。
    pub(super) fn body_ref_issued(&self, window_id: &str, body_ref: &str) -> bool {
        self.windows
            .get(window_id)
            .is_some_and(|record| record.issued_body_refs.contains(body_ref))
    }

    /// 关闭某会话在本连接的全部窗口（含仍在 `Opening` 的占位），并记录 closed 身份。
    ///
    /// 返回移除数量。其他会话窗口不受影响。
    pub(super) fn remove_session(
        &mut self,
        session_id: &str,
        now: Duration,
        record_closed: bool,
    ) -> usize {
        self.remove_session_except(session_id, None, now, record_closed)
    }

    /// 关闭某会话除 `keep` 以外的全部窗口；用于 initial 提交时替换旧窗口。
    ///
    /// `keep` 是刚完成准备、尚未发布的候选窗口；只有提交成功后才调用。返回移除数量。
    pub(super) fn remove_session_except(
        &mut self,
        session_id: &str,
        keep: Option<&str>,
        now: Duration,
        record_closed: bool,
    ) -> usize {
        let ids: Vec<String> = self
            .windows
            .iter()
            .filter(|(id, record)| {
                record.session_id == session_id && keep.is_none_or(|keep| *id != keep)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let removed = ids.len();
        for id in ids {
            self.remove(&id, now, record_closed);
        }
        removed
    }

    /// 丢弃一个尚未发布的窗口（候选回收）：不记录 closed 身份，不影响其他窗口。
    pub(super) fn discard(&mut self, window_id: &str) -> bool {
        let Some(record) = self.windows.remove(window_id) else {
            return false;
        };
        record.cancel.cancel(false);
        true
    }

    /// 窗口的在途读取消信号；窗口已被移除时返回 `None`。
    pub(super) fn cancel_handle(&self, window_id: &str) -> Option<WindowCancel> {
        self.windows
            .get(window_id)
            .map(|record| record.cancel_handle())
    }

    /// 取消清理：只移除仍处于 `Opening` 的占位，已完成的窗口不受影响。
    ///
    /// 由 `OpeningGuard::drop` 在同步上下文调用；不记录 closed 身份（从未对客户端
    /// 发布过该窗口）。
    pub(super) fn remove_if_opening(&mut self, window_id: &str) -> bool {
        let Some(record) = self.windows.get(window_id) else {
            return false;
        };
        if record.snapshot.is_some() {
            return false;
        }
        if let Some(record) = self.windows.remove(window_id) {
            record.cancel.cancel(false);
        }
        true
    }

    /// 移除窗口；显式 close 记录身份，失效/过期不记录。
    pub(super) fn remove(&mut self, window_id: &str, now: Duration, record_closed: bool) {
        let Some(record) = self.windows.remove(window_id) else {
            return;
        };
        // 关闭/移除撤销该窗口的在途读；`record_closed` 决定迟到结果的领域原因。
        record.cancel.cancel(record_closed);
        if record_closed {
            self.closed.push_back(ClosedWindow {
                window_id: window_id.to_owned(),
                closed_at: now,
            });
        }
    }

    pub(super) fn was_closed(&self, window_id: &str) -> bool {
        self.closed
            .iter()
            .any(|closed| closed.window_id == window_id)
    }
}

pub(super) fn new_window_id() -> String {
    format!("w_{}", uuid::Uuid::now_v7())
}

pub(super) fn new_token(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}
