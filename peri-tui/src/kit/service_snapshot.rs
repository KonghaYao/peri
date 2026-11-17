//! Service snapshot 的周期协调器。
//!
//! 这个协调器只负责按既有节奏采样源、调用各责任单元并把观察结果交给统一
//! 提交门；它不在渲染期产生副作用。文件、memory、会话列表、会话元数据和会话
//! 服务各自维护 last-good/节流状态；提交门负责一次身份重验和类型明确的部分发布。
//! 本批只拆职责，仍串行完成单次 tick，不引入独立调度或新增查询；慢任务不会
//! 叠加相同 key 的刷新，但仍会等待本 tick 其它观察值。

mod commit;
mod file_scan;
mod mcp_refresh;
mod memory_scan;
mod session_metadata;
mod session_services;
mod thread_list_refresh;

#[cfg(test)]
#[path = "service_snapshot_test.rs"]
mod tests;

#[cfg(test)]
use crate::acp_client::AcpTuiClient;
#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use session_metadata::derive_provider_and_model;

mod refresh;
pub use refresh::*;

pub(crate) use commit::{SessionFieldUpdate, publish_session_fields};
pub(crate) use mcp_refresh::refresh_mcp_projection;

#[cfg(test)]
use crate::kit::atoms::ACTIVE_EXECUTION_CWD;

#[cfg(test)]
use crate::kit::atoms::ACTIVE_SESSION_ID;

#[cfg(test)]
use crate::kit::atoms::THREAD_BROWSER_ARCHIVED;

#[cfg(test)]
use crate::kit::atoms::THREAD_BROWSER_SCOPE;

#[cfg(test)]
use crate::kit::atoms::THREAD_LIST_ERROR;

#[cfg(test)]
use crate::kit::atoms::THREAD_LIST_PAGE_COUNT;
