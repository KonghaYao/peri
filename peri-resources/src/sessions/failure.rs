//! 与具体存储驱动无关的会话失败分类。
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
#[cfg(target_os = "emscripten")]
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

pub(in crate::sessions) fn read_only_store() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::ReadOnlyStore)
}

/// 与驱动无关的执行失败映射：保留领域和 workspace 失败，其余映射为后端暂不可用。
/// SQLite 的 SQLx 失败仍由其 adapter 按具体原因分类。
#[cfg(target_os = "emscripten")]
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
