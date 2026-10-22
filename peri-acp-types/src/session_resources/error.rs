//! 会话行为失败：原因、效果确定性与窗口错误映射。
//!
//! 从 `session_resources.rs` 按职责拆出：错误分类与效果规则独立于 trait 门面，
//! 公开路径 `peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind}`
//! 由父模块 `pub use` 保持。

use crate::history_page::HistoryCursorError;
use crate::history_window::HistoryWindowError;
use crate::thread::ThreadId;
use crate::workspace::WorkspaceError;

use super::MutationOutcome;

/// 会话行为的统一返回类型。
pub type SessionResourceResult<T> = std::result::Result<T, SessionResourceError>;

/// Readback of an uncertain close-intent removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseSettlement {
    Pending,
    Finished,
    Unknown,
}

/// 失败原因。不携带 SQL、token、事务 ID 或连接细节。
#[derive(Debug)]
pub enum SessionResourceErrorKind {
    InvalidInput {
        detail: String,
    },
    NotFound,
    /// 后端能力面不支持该行为（含只读能力面下的 mutation）。
    Unsupported,
    /// 本次打开只有读权限。
    ReadOnlyStore,
    /// 本机 workspace / binding 语义，保持 [`WorkspaceError`] 的分类。
    Workspace(WorkspaceError),
    /// 记录存在但无法解释（损坏、格式不可读）。
    Corrupt {
        detail: String,
    },
    /// 派生索引缺失或不完整，且回退到全量扫描会超出有界预算。
    ///
    /// 读取请求不得隐式建索引或扫描整列；出口应返回 `index_not_ready`，
    /// 由调用方稍后重试或显式准备索引，不能降级成空页。
    IndexNotReady {
        detail: String,
    },
    /// 后端暂不可用（网络、锁、IO）：可重试，但不代表没生效。
    Unavailable {
        detail: String,
    },
    Timeout,
    /// 无法证明上一次写入的终态；携带可定位的 session/root 关联。
    PersistenceUncertain {
        thread_id: Option<ThreadId>,
    },

    /// 数据已完整保存，但执行准入失败：不是「确定未创建」。
    SavedButNotAdmitted {
        thread_id: ThreadId,
    },
    /// 现有状态与本次请求冲突（一次性提交重复调用、对已定稿草稿撤销、清理判据不成立）。
    ///
    /// 与 [`Self::InvalidInput`] 分开：输入本身合法，冲突来自存储里已经成立的事实，
    /// 调用方据此知道「不要重试，先重读」。
    Conflict {
        detail: String,
    },
    /// 历史分页游标不能用于本次读取：分类见 [`HistoryCursorError`]。
    ///
    /// 单独一类是为了让出口把「坏游标」映射成参数错误、把「失效游标」映射成需要重新
    /// 开始的错误；两者都不得降级成首屏或空页。
    Cursor(HistoryCursorError),
    /// `peri.historyWindowV1` 的稳定错误分类；出口按
    /// [`HistoryWindowError::code`]/[`HistoryWindowError::reason`] 映射，不解析文本。
    HistoryWindow(HistoryWindowError),
    Internal {
        detail: String,
    },
}

/// 会话行为失败：原因 + 效果确定性。
///
/// `effect` 由构造器从 `kind` 派生，调用方无法拼出互相矛盾的组合：
/// `Unknown` 只对应未决持久化，`Applied` 只对应「已保存但准入失败」。
#[derive(Debug)]
pub struct SessionResourceError {
    kind: SessionResourceErrorKind,
    effect: MutationOutcome,
}

impl SessionResourceError {
    pub fn new(kind: SessionResourceErrorKind) -> Self {
        let effect = match &kind {
            SessionResourceErrorKind::PersistenceUncertain { .. } => MutationOutcome::Unknown,
            SessionResourceErrorKind::SavedButNotAdmitted { .. } => MutationOutcome::Applied,
            _ => MutationOutcome::NotApplied,
        };
        Self { kind, effect }
    }

    /// 无法证明写入终态：门面据此使热态失效并阻塞续写/clean。
    pub fn persistence_uncertain(thread_id: Option<ThreadId>) -> Self {
        Self::new(SessionResourceErrorKind::PersistenceUncertain { thread_id })
    }

    /// 数据已完整保存，但执行准入失败；包含可定位的 session identity。
    pub fn saved_but_not_admitted(thread_id: ThreadId) -> Self {
        Self::new(SessionResourceErrorKind::SavedButNotAdmitted { thread_id })
    }

    /// 本次请求与已成立的存储事实冲突（不覆盖、不删除、不重试）。
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(SessionResourceErrorKind::Conflict {
            detail: detail.into(),
        })
    }

    pub fn kind(&self) -> &SessionResourceErrorKind {
        &self.kind
    }

    pub fn effect(&self) -> MutationOutcome {
        self.effect
    }

    pub fn is_persistence_uncertain(&self) -> bool {
        matches!(
            self.kind,
            SessionResourceErrorKind::PersistenceUncertain { .. }
        )
    }

    /// 仅当失败来源于本机 workspace 语义时给出原错误，供只读降级等既有映射使用。
    pub fn workspace_error(&self) -> Option<&WorkspaceError> {
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => Some(error),
            _ => None,
        }
    }

    /// 窗口读取失败时给出原分类；出口据此选择 V1 错误码。
    pub fn history_window_error(&self) -> Option<&HistoryWindowError> {
        match &self.kind {
            SessionResourceErrorKind::HistoryWindow(error) => Some(error),
            _ => None,
        }
    }
}

impl From<WorkspaceError> for SessionResourceError {
    fn from(error: WorkspaceError) -> Self {
        Self::new(SessionResourceErrorKind::Workspace(error))
    }
}

impl From<HistoryWindowError> for SessionResourceError {
    fn from(error: HistoryWindowError) -> Self {
        Self::new(SessionResourceErrorKind::HistoryWindow(error))
    }
}

impl std::fmt::Display for SessionResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (reason, detail) = match &self.kind {
            SessionResourceErrorKind::InvalidInput { detail } => ("invalid input", detail.as_str()),
            SessionResourceErrorKind::NotFound => ("session not found", ""),
            SessionResourceErrorKind::Unsupported => ("behavior is unsupported here", ""),
            SessionResourceErrorKind::ReadOnlyStore => ("session store is read-only", ""),
            SessionResourceErrorKind::Workspace(_) => ("workspace check failed", ""),
            SessionResourceErrorKind::Corrupt { detail } => {
                ("session data is corrupt", detail.as_str())
            }
            SessionResourceErrorKind::IndexNotReady { detail } => {
                ("history index is not ready", detail.as_str())
            }
            SessionResourceErrorKind::Unavailable { detail } => {
                ("store is unavailable", detail.as_str())
            }
            SessionResourceErrorKind::Timeout => ("store operation timed out", ""),
            SessionResourceErrorKind::PersistenceUncertain { .. } => {
                ("persistence outcome is unknown; reload the session", "")
            }
            SessionResourceErrorKind::SavedButNotAdmitted { .. } => {
                ("session data was saved but execution admission failed", "")
            }
            SessionResourceErrorKind::Conflict { detail } => {
                ("state conflicts with request", detail.as_str())
            }
            SessionResourceErrorKind::Cursor(_) => ("history cursor is unusable", ""),
            SessionResourceErrorKind::HistoryWindow(_) => ("history window read failed", ""),
            SessionResourceErrorKind::Internal { detail } => {
                ("internal storage error", detail.as_str())
            }
        };
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => write!(f, "{reason}: {error}"),
            SessionResourceErrorKind::Cursor(error) => write!(f, "{reason}: {error}"),
            SessionResourceErrorKind::HistoryWindow(error) => write!(f, "{reason}: {error}"),
            _ if detail.is_empty() => {
                write!(f, "{reason} ({:?})", self.effect)
            }
            _ => write!(f, "{reason}: {detail}"),
        }
    }
}

impl std::error::Error for SessionResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            SessionResourceErrorKind::Workspace(error) => Some(error),
            SessionResourceErrorKind::HistoryWindow(error) => Some(error),
            _ => None,
        }
    }
}
