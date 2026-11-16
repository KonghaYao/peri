//! `peri.historyWindowV1` Host coordinator 与 open/page/seek/body 路由。
//!
//! 执行顺序（计划 §5.2）：连接级能力/schema → sessions 短锁取 generation/structure/ticket
//! → **锁外**调用 `SessionResources` → 返回前短锁二次校验。任何 DB I/O 都不在 sessions
//! 全局锁内；窗口读取不取 prompt lock、不广播、不装配、不刷新 input queue。
//!
//! coordinator 只保存轻量事实：ticket 绑定、snapshot/tail/checkpoint、rewind 状态与
//! in-flight 计数。历史正文始终在存储侧，响应按页装配后立即释放。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use peri_acp_types::history_window::{
    HistoryCheckpoint, HistoryCommittedTail, HistoryEntryId, HistoryInheritedSnapshot,
    HistoryReadTicket, HistoryReadTicketBinding, HistorySource, HistoryWindowCursor,
    HistoryWindowDirection, HistoryWindowError, HistoryWindowMetadata, HistoryWindowOpenRequest,
    HistoryWindowPageRequest, HistoryWindowRequest, HistoryWindowSeekRequest,
    HISTORY_WINDOW_DEFAULT_BYTE_BUDGET, HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
    HISTORY_WINDOW_MAX_CHUNK_SIZE, HISTORY_WINDOW_PROJECTION_VERSION,
};
use peri_acp_types::session_resources::HistoryWindowPagePlan;
use peri_acp_types::session_resources::HistoryWindowReadIntent;
use peri_acp_types::session_resources::HistoryWindowTailExpectation;
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;

use crate::transport::types::AcpError;

use super::{AcpServerConfig, SessionState, SharedSessions};

pub(crate) mod child;
mod child_display;
mod commit_observer;
mod inherited;
mod oversize_memory;
use oversize_memory as oversize;
mod state;
mod visibility;
mod wire;

#[cfg(test)]
mod commit_tail_test;

pub(crate) use commit_observer::StoreCompactCommitObserver;

#[cfg(test)]
pub(crate) use history_window_coordinator::WindowNotificationSink;

pub(crate) use history_window_coordinator::WindowConnection;

pub(crate) use history_window_coordinator::TransportWindowSink;

pub(crate) use history_window_coordinator::IssuedWindowTicket;

pub(crate) use history_window_coordinator::HistoryWindowCoordinator;

pub(crate) use history_window_coordinator::window_error;

pub(crate) use history_window_coordinator::resource_window_error;

pub(crate) use history_window_coordinator::handle;

pub(crate) use history_window_coordinator::handle_body_chunk;

pub(crate) use history_window_coordinator::notify_commit_if_open;

pub(crate) use history_window_coordinator::publish_commit;

pub(crate) use history_window_coordinator::revoke;

pub(crate) use history_window_coordinator::invalidate;

pub(crate) use history_window_coordinator::begin_rewind;

pub(crate) use history_window_coordinator::finish_rewind;

#[path = "history_window/history_window_coordinator.rs"]
mod history_window_coordinator;
