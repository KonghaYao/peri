//! 窗口创建：open 与 initial 共享同一套占位、容量、快照复核和票据注册。
//!
//! 调用方负责幂等账本；本模块只在后端读取成功后提交窗口。任何失败路径都由
//! `OpeningGuard` 清掉仍处于 `Opening` 的占位，不会把 ticket/window 泄漏给客户端。

use peri_acp_types::history_v2::{
    HistoryAnchor, HistoryPageData, HistorySnapshotRef, HistoryV2Backend,
};

use super::error::ServiceFailure;
use super::state::{new_token, new_window_id, WindowAccess, WindowRecord};
use super::{window_access, HistoryV2Service, OpeningGuard};

/// 已提交的窗口与首屏数据；页面装配仍由调用方完成。
pub(super) struct OpenedWindow {
    pub(super) access: WindowAccess,
    pub(super) data: HistoryPageData,
}

impl HistoryV2Service {
    /// 预留窗口槽位、调用后端 open、复核 tail/revision，并注册快照与 home cursor。
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
    ) -> Result<OpenedWindow, ServiceFailure> {
        let window_id = new_window_id();
        let snapshot_token = new_token("st");
        let read_ticket = new_token("rt");
        let now = self.clock.now();
        {
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
                )
                .map_err(|()| ServiceFailure::capacity_exceeded())?;
        }
        let mut opening = OpeningGuard::new(&self.state, window_id.clone());

        let opened = backend
            .open(session_id, anchor, before, after, target_bytes)
            .await
            .map_err(ServiceFailure::from_backend)?;
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
}
