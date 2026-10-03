//! 与具体存储驱动无关的会话失败分类。
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_acp_types::workspace::WorkspaceError;

pub(in crate::sessions) fn invalid_input(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.to_owned(),
    })
}

pub(in crate::sessions) fn corrupt(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Corrupt {
        detail: detail.to_owned(),
    })
}

pub(in crate::sessions) fn unavailable(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unavailable {
        detail: detail.to_owned(),
    })
}

pub(in crate::sessions) fn not_found() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::NotFound)
}

/// 现有存储事实与本次请求冲突：一次性提交的重复调用、对已定稿草稿的撤销、
/// 清理判据不成立。语义是「不要重试，先重读」，与输入非法分开。
pub(in crate::sessions) fn conflict(detail: &str) -> SessionResourceError {
    SessionResourceError::conflict(detail)
}

pub(in crate::sessions) fn read_only_store() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::ReadOnlyStore)
}

/// 「需要一条活的本机执行所有者」这一领域的统一失败。
pub(in crate::sessions) fn lease_required() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Workspace(
        WorkspaceError::ExecutionLeaseRequired,
    ))
}

/// 本机执行面失败映射：领域失败（已带效果）原样保留，workspace 语义同样保留，SQL 失败按
/// 原因分类，其余（IO、发现探测等）都是「后端暂不可用」，不冒充「没有这条会话」。
#[cfg(any(test, target_os = "emscripten"))]
pub(in crate::sessions) fn execution_failure(error: anyhow::Error) -> SessionResourceError {
    match error.downcast::<SessionResourceError>() {
        Ok(domain) => domain,
        Err(error) => {
            if let Some(workspace) = error.downcast_ref::<WorkspaceError>() {
                return SessionResourceError::new(SessionResourceErrorKind::Workspace(
                    workspace.clone(),
                ));
            }
            unavailable("local session execution state is unavailable")
        }
    }
}
