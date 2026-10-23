//! 窗口创建：open 与 initial 共享同一套占位、容量、快照复核和票据注册。
//!
//! 创建拆成两段（FIX-05）：
//! - [`HistoryV2Service::reserve_window`]：同步预留窗口槽位并建立 `Opening` 占位；
//! - [`HistoryV2Service::open_window`]：在锁外调用后端、复核 revision、升级为 `Ready`。
//!
//! initial 在两者之间创建实时候选流，使 `backend.open` 期间的事件进入候选缓冲，而旧流
//! 的身份与序号保持不变。任何失败路径都由 `OpeningGuard` 清掉仍处于 `Opening` 的占位，
//! 不会把 ticket/window 泄漏给客户端。

use peri_acp_types::history_v2::{
    HistoryAnchor, HistoryPageData, HistorySnapshotRef, HistoryV2Backend,
};

use super::error::ServiceFailure;
use super::state::{new_token, new_window_id, WindowAccess, WindowCancel, WindowRecord};
use super::{window_access, HistoryV2Service, OpeningGuard};

/// 已提交的窗口与首屏数据；页面装配仍由调用方完成。
pub(super) struct OpenedWindow {
    pub(super) access: WindowAccess,
    pub(super) data: HistoryPageData,
}

/// 已预留、尚未读取后端的窗口占位。
///
/// 持有 `OpeningGuard`：调用方在 `reserve_window` 与 `open_window` 之间 drop 时，
/// 占位会被同步移除；`open_window` 成功或明确失败后由内部 disarm。
pub(super) struct ReservedWindow<'a> {
    window_id: String,
    cancel: WindowCancel,
    opening: OpeningGuard<'a>,
}

impl HistoryV2Service {
    /// 同步预留窗口槽位（不读后端）。
    ///
    /// `allow_replacement` 只由 initial 传入：同会话已有窗口且总数达到 `max` 时，
    /// 允许一个额外候选槽（`max + 1`），成功后由提交阶段关闭旧窗口。
    pub(super) fn reserve_window(
        &self,
        session_id: &str,
        max_windows: u32,
        allow_replacement: bool,
    ) -> Result<ReservedWindow<'_>, ServiceFailure> {
        let window_id = new_window_id();
        let snapshot_token = new_token("st");
        let read_ticket = new_token("rt");
        let now = self.clock.now();
        let cancel = {
            let mut state = self.state.lock();
            state.windows.prune(now);
            state
                .windows
                .reserve(
                    WindowRecord::opening(
                        window_id.clone(),
                        snapshot_token,
                        read_ticket,
                        session_id.to_owned(),
                        now,
                    ),
                    max_windows,
                    allow_replacement,
                )
                .map_err(|()| ServiceFailure::capacity_exceeded())?;
            state.windows.cancel_handle(&window_id)
        };
        let Some(cancel) = cancel else {
            return Err(ServiceFailure::window_expired());
        };
        let opening = OpeningGuard::new(&self.state, window_id.clone());
        Ok(ReservedWindow {
            window_id,
            cancel,
            opening,
        })
    }

    /// 调用后端 open 并完成窗口快照注册。
    ///
    /// 窗口在读取期间被 close/过期/会话拆除时，取消后端读取并显式失败。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn open_window(
        &self,
        reserved: ReservedWindow<'_>,
        backend: &dyn HistoryV2Backend,
        session_id: &str,
        anchor: &HistoryAnchor,
        before: u32,
        after: u32,
        target_bytes: u64,
    ) -> Result<OpenedWindow, ServiceFailure> {
        let ReservedWindow {
            window_id,
            cancel,
            mut opening,
        } = reserved;

        let opened = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(if cancel.was_closed() {
                    ServiceFailure::window_closed()
                } else {
                    ServiceFailure::window_expired()
                });
            }
            opened = backend.open(session_id, anchor, before, after, target_bytes) => opened,
        };
        let opened = opened.map_err(ServiceFailure::from_backend)?;
        if opened.page.history_revision != opened.history_revision
            || opened.page.projection_version != opened.projection_version
            || opened.page.committed_tail != opened.committed_tail
        {
            return Err(ServiceFailure::snapshot_invalidated());
        }

        let snapshot = HistorySnapshotRef {
            session_id: session_id.to_owned(),
            committed_tail: opened.committed_tail,
            history_revision: opened.history_revision,
            projection_version: opened.projection_version,
            target_bytes,
        };
        let now = self.clock.now();
        let access = {
            let mut state = self.state.lock();
            state.windows.prune(now);
            if !state.windows.complete(
                &window_id,
                snapshot,
                opened.page.before_cursor.clone(),
                opened.page.after_cursor.clone(),
                now,
            ) {
                return Err(ServiceFailure::window_expired());
            }
            let Some(access) = window_access(&state, &window_id) else {
                state.windows.remove(&window_id, now, false);
                return Err(ServiceFailure::window_expired());
            };
            access
        };
        opening.disarm();
        Ok(OpenedWindow {
            access,
            data: opened.page,
        })
    }

    /// 预留 + 后端 open 的组合入口（只读 `open` 使用；initial 需要两段之间插候选流）。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn begin_window(
        &self,
        backend: &dyn HistoryV2Backend,
        session_id: &str,
        anchor: &HistoryAnchor,
        before: u32,
        after: u32,
        target_bytes: u64,
        max_windows: u32,
        allow_replacement: bool,
    ) -> Result<OpenedWindow, ServiceFailure> {
        let reserved = self.reserve_window(session_id, max_windows, allow_replacement)?;
        self.open_window(
            reserved,
            backend,
            session_id,
            anchor,
            before,
            after,
            target_bytes,
        )
        .await
    }
}
