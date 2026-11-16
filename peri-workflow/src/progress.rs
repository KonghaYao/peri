//! Reducer-based progress store: processes `ProgressEvent` and maintains UI-queryable state.
//!
//! Key design: agentId EXACT matching (not LIFO stack) — concurrent agents interleave
//! events, so `set_or_update_agent` finds by `agent_id` field, not the last element.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::protocol::{AgentRunResult, ProgressEvent};

pub use workflow_progress_store::ProgressObserver;

pub use workflow_progress_store::RunProgress;

// 3.0 批 2 波 1：协议投影类型迁入契约层 `peri_acp_types::workflow`
// （`RunStatus` / `WorkflowMeta` / `MetaPhase` / `PhaseProgress` /
// `PhaseStatus` / `AgentProgress` / `AgentStatus` / `PhaseSummary`）；
// 本模块保留 re-export 保兼容。`RunProgress`（含 `IndexMap` 字段）保留在本文件。
pub use peri_acp_types::workflow::{
    AgentProgress, AgentStatus, MetaPhase, PhaseProgress, PhaseStatus, PhaseSummary, RunStatus,
    WorkflowMeta,
};

pub use workflow_progress_store::WorkflowProgressStore;

// ─── Serde helper: serialize IndexMap<u64, AgentProgress> as JSON array ───

#[path = "progress/agents_as_map.rs"]
mod agents_as_map;

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;

#[path = "progress/workflow_progress_store.rs"]
mod workflow_progress_store;
