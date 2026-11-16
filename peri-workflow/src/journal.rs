//! 磁盘持久化：workflow 运行日志、状态快照、脚本副本。
//!
//! 目录结构：`.claude/workflow-runs/<runId>/`
//! - `journal.jsonl` — append-only agent() 调用结果日志（用于 cache-hit resume）
//! - `state.json` — 最终状态快照（run_done 时原子写入）
//! - `script.js` — workflow 脚本源码副本

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use peri_acp_types::workflow::{
    AcceptanceStatus, DeliveryStatus, ExecutionStatus, PostProcessingStatus,
};
use serde::{Deserialize, Serialize};

use crate::protocol::JournalEntry;

mod git_baseline;
use git_baseline as git;
mod output;
pub use git::GitBaseline;
pub use output::extract_long_texts;

pub use workflow_journal_store::RunState;

pub use workflow_journal_store::WorkflowJournalStore;

#[cfg(test)]
#[path = "journal_test.rs"]
mod tests;

#[path = "journal/workflow_journal_store.rs"]
mod workflow_journal_store;
