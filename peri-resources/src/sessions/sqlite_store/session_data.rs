use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::database::SqliteSessionDatabase;
use super::failure::{
    commit_failure, corrupt, invalid_input, map_sqlx, not_found, read_failure, unavailable,
    write_failure,
};
use super::row_mapping::extract_title;
use super::session_rows::{
    delete_thread_child_rows, insert_binding_row, insert_thread_row, ThreadRowInsert,
};
use super::{compaction, context, session_rows, workspace as workspace_store};
use crate::sessions::canonical::payload_role;
use crate::sessions::data::ensure_child_relation;
use crate::sessions::data::ChildResumeRecord;
use crate::sessions::data::SessionDataPort;
use async_trait::async_trait;
use peri_acp_types::history_page::{
    message_id_of, resolve_browsing_range, HistoryCursor, HistoryCursorError, HistoryForwardCursor,
    HistoryPage, HistoryPageBounds, HistoryPageCandidate, HistoryPagePlan, HistoryPageRow,
    HistorySegment, HistorySegmentPlan,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, BindingState, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionDraft, PersistenceRecovery,
    RewindBoundary, RewindCandidate, RewindOutcome, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult, SessionSnapshot,
};
use peri_acp_types::store::{
    deserialize_persisted_payload, serialize_persisted_payload, CompactionChange, InheritedContext,
    MessageFlags, PersistedPayload,
};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, WorkspaceError,
    SESSION_BINDING_VERSION,
};
use sqlx::SqliteConnection;

#[path = "session_data/archive.rs"]
mod archive;
#[path = "session_data/async_task.rs"]
mod async_task;
#[path = "session_data/blob_reader.rs"]
mod blob_reader;
#[path = "session_data/catalog.rs"]
mod catalog;
#[path = "session_data/history.rs"]
mod history;
#[path = "session_data/history_projection.rs"]
mod history_projection;
#[path = "session_data/history_window.rs"]
mod history_window;
#[path = "session_data/inherited_blob.rs"]
mod inherited_blob;
#[path = "session_data/inherited_facts.rs"]
mod inherited_facts;
#[path = "session_data/inherited_index.rs"]
pub(super) mod inherited_index;
#[path = "session_data/inherited_page.rs"]
mod inherited_page;
#[path = "session_data/lifecycle.rs"]
mod lifecycle;
#[path = "session_data/rewind_candidates.rs"]
mod rewind_candidates;

#[path = "session_data/handle.rs"]
mod handle;
pub(crate) use handle::SqliteSessionData;
#[path = "session_data/port.rs"]
mod port;

// ─── 行读取原语 ───────────────────────────────────────────────────────────────

#[path = "session_data_helpers.rs"]
mod helpers;
use helpers::*;
pub(super) use helpers::{
    message_insert_chunk_rows, new_session_draft_row, new_session_row,
    validate_unbound_legacy_frozen,
};

/// 有界读取的性能证据（需求 §9 A04/A05/A06/A07）：默认 `#[ignore]`，不拖慢默认测试。
///
/// 运行命令、原始输出与数字见 `.recon/perf-evidence.md`。
#[cfg(test)]
#[path = "session_history_perf_test.rs"]
mod history_perf_tests;

#[cfg(test)]
#[path = "session_data/history_window_incremental_test.rs"]
mod history_window_incremental_tests;

#[cfg(test)]
#[path = "session_data/inherited_facts_test.rs"]
mod inherited_facts_tests;

#[cfg(test)]
#[path = "session_data/inherited_index_test.rs"]
mod inherited_index_tests;

#[cfg(test)]
#[path = "session_data/inherited_perf_test.rs"]
mod inherited_perf_tests;

#[cfg(test)]
#[path = "session_data/history_window_layout_test.rs"]
mod history_window_layout_tests;
