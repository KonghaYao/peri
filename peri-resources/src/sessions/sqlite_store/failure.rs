//! 私有失败映射：`sqlx` / `anyhow` → 领域失败。
//!
//! 数据面（[`super::session_data`]）、本机执行面（[`super::local`]）与门面
//! （[`crate::sessions::SessionResourcesImpl`]）共用同一套分类，避免同一种失败在三处
//! 各自解释。这里只回答「原因是什么」，不决定效果确定性——那是行为层的判断
//! （见 `MutationOutcome`）。
//!
//! 映射规则固定为：会话行缺失是 `NotFound`；本机 workspace 语义原样保留变体；
//! 唯一键冲突是「identity 已存在」；外键失败是绑定关系问题；解码/列形状失败是
//! 数据读不懂；其余 SQL 失败是「后端暂时不可用」，不冒充「没生效」。
//!
//! 唯一例外是写事务的提交阶段（[`commit_failure`]）：一旦进入 `commit()`，错误形状不再
//! 能回答问题，「没生效」也不能由原因推出，因此固定上报未决持久化。

pub(in crate::sessions) use crate::sessions::failure::{
    corrupt, invalid_input, not_found, unavailable,
};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::WorkspaceError;

/// SQLx 失败 → 领域失败：只给稳定分类，不把 SQL 文本或驱动消息带出实现。
pub(in crate::sessions) fn map_sqlx(error: &sqlx::Error) -> SessionResourceError {
    match error {
        sqlx::Error::RowNotFound => not_found(),
        sqlx::Error::Database(database) => {
            if database.is_unique_violation() {
                invalid_input("session identity already exists")
            } else if database.is_foreign_key_violation() {
                SessionResourceError::new(SessionResourceErrorKind::Workspace(
                    WorkspaceError::InvalidBinding,
                ))
            } else if database.is_check_violation() {
                corrupt("stored value violates its column constraints")
            } else {
                unavailable("session database rejected the write")
            }
        }
        sqlx::Error::Decode(_)
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::ColumnNotFound(_) => {
            corrupt("stored session data does not match its column types")
        }
        _ => unavailable("session database is unavailable"),
    }
}

/// 写事务进入 `commit()` 之后的失败：不再能证明「未生效」，一律上报未决持久化。
///
/// `sqlx` 在提交失败后由连接回滚，但「回滚是否真的完成」不由调用方观察得到；底层错误
/// 长得像 [`SessionResourceErrorKind::Unavailable`]（IO、驱动未分类失败）时尤其不能冒充
/// 「没写进去」。因此提交阶段不做原因分类，效果固定为
/// [`MutationOutcome::Unknown`](peri_acp_types::session_resources::MutationOutcome)：
/// 写入准入据此不结清范围，由门禁保留未决证据，阻断续写与关闭确认。
///
/// 只用于**写事务**的 `commit()`：提交之前的失败（输入非法、约束冲突、可证明的回滚）
/// 仍走 [`write_failure`] / [`map_sqlx`]，不得被判成 `Unknown`；只读事务没有写入效果，
/// 也不走这里。
pub(in crate::sessions) fn commit_failure(thread_id: Option<ThreadId>) -> SessionResourceError {
    SessionResourceError::persistence_uncertain(thread_id)
}

/// 读取路径失败映射：会话行缺失是 `NotFound`，其余（含解码、投影、祖先链损坏）
/// 都是「记录在但读不懂」。
pub(in crate::sessions) fn read_failure(error: anyhow::Error) -> SessionResourceError {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sql_error) => map_sqlx(sql_error),
        None => corrupt("stored session data is not readable"),
    }
}

/// 已经带效果的领域失败优先保留：`Unknown`（提交未决）与 `Applied`（已保存未准入）经
/// `anyhow` 传播后（compaction 事务、本机执行面），不得再按错误形状降级成「没生效」。
pub(in crate::sessions) fn preserve_domain_failure(
    error: anyhow::Error,
) -> Result<SessionResourceError, anyhow::Error> {
    error.downcast::<SessionResourceError>()
}

/// 写入路径失败映射：领域失败（已带效果）原样保留，绑定形状/关系失败保持 workspace 语义。
pub(in crate::sessions) fn write_failure(error: anyhow::Error) -> SessionResourceError {
    match preserve_domain_failure(error) {
        Ok(domain) => domain,
        Err(error) => match error.downcast_ref::<WorkspaceError>() {
            Some(workspace) => {
                SessionResourceError::new(SessionResourceErrorKind::Workspace(workspace.clone()))
            }
            None => match error.downcast_ref::<sqlx::Error>() {
                Some(sql_error) => map_sqlx(sql_error),
                None => corrupt("stored session data is not writable"),
            },
        },
    }
}

/// Native execution failures retain the SQLx-specific classification at the adapter boundary.
pub(in crate::sessions) fn execution_failure(error: anyhow::Error) -> SessionResourceError {
    match preserve_domain_failure(error) {
        Ok(domain) => domain,
        Err(error) => {
            if let Some(workspace) = error.downcast_ref::<WorkspaceError>() {
                return SessionResourceError::new(SessionResourceErrorKind::Workspace(
                    workspace.clone(),
                ));
            }
            match error.downcast_ref::<sqlx::Error>() {
                Some(sql_error) => map_sqlx(sql_error),
                None => unavailable("local session execution state is unavailable"),
            }
        }
    }
}

/// 绑定关系复核失败映射：本机登记不存在（`RowNotFound`）是绑定无效，不是「会话不存在」。
pub(in crate::sessions) fn binding_relation_failure(error: anyhow::Error) -> SessionResourceError {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::RowNotFound) => SessionResourceError::new(
            SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding),
        ),
        _ => write_failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::session_resources::MutationOutcome;

    /// 提交阶段漏分类的形态：底层失败（IO / 驱动未分类）经 [`map_sqlx`] 得到
    /// `Unavailable`，效果是 `NotApplied`——那是「没生效」的假证据。写事务提交阶段的
    /// 失败必须改判 `Unknown`，让写入准入保留未决证据并阻断续写与 clean。
    #[test]
    fn test_commit_stage_failure_is_unknown_not_a_not_applied_proof() {
        let io = sqlx::Error::Io(std::io::Error::other("commit response lost"));
        assert_eq!(map_sqlx(&io).effect(), MutationOutcome::NotApplied);

        let error = commit_failure(Some("s-commit".to_owned()));
        assert!(error.is_persistence_uncertain());
        assert_eq!(error.effect(), MutationOutcome::Unknown);
        assert_eq!(commit_failure(None).effect(), MutationOutcome::Unknown);
    }

    /// `anyhow` 传播（compaction 事务、本机执行面）不能把已定效果的领域失败降级：
    /// 若 `write_failure` / `execution_failure` 仍按错误形状分类，提交未决会在第二次
    /// 映射里变成 `NotApplied` 并放开写入准入。
    #[test]
    fn test_domain_unknown_survives_anyhow_mapping() {
        let unknown = || anyhow::Error::new(commit_failure(Some("s-anyhow".to_owned())));
        assert_eq!(write_failure(unknown()).effect(), MutationOutcome::Unknown);
        assert!(write_failure(unknown()).is_persistence_uncertain());
        assert_eq!(
            execution_failure(unknown()).effect(),
            MutationOutcome::Unknown
        );
    }
}
