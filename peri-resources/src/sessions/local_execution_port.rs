//! 工作区发现与绑定校验端口；不管理会话执行所有权。

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, WorkspaceError, WorkspaceId};

/// 工作区发现与保存证据的校验接缝，数据持久化由 SessionDataPort 负责。
#[async_trait]
pub(in crate::sessions) trait LocalExecutionPort: Send + Sync {
    /// 本机是否只读打开（只读时禁止写入，历史仍可读）。
    fn is_read_only(&self) -> bool;

    /// 当前执行环境的稳定机器身份；门面只比较持久归属，不读取进程全局身份。
    fn machine_id(&self) -> Result<String> {
        Ok(super::machine::current()?.to_owned())
    }

    /// 保存的 cwd 在当前执行环境中是否仍可作为目录使用。
    async fn directory_available(&self, cwd: &Path) -> bool {
        #[cfg(target_os = "emscripten")]
        {
            let _ = cwd;
            false
        }
        #[cfg(not(target_os = "emscripten"))]
        {
            tokio::fs::metadata(cwd)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        }
    }

    // ── 发现与登记 ──

    /// 解析并登记本机执行目录。
    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace>;

    /// 把无绑定会话的历史归属收敛到本机可证实的 Workspace（幂等修复，可重跑）。
    ///
    /// 只在会话没有绑定行、原归属行从未被本机观测过（无执行证据），且目标归属带着
    /// 本次观测证据时改写 `threads.workspace_id`；其余情况如实失败，不静默改绑。
    /// 远端组合不提供这项能力（legacy 接纳只在本机发生）。
    async fn relink_unbound_ownership(
        &self,
        _id: &ThreadId,
        _workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        Err(WorkspaceError::UnverifiedLegacyOwner.into())
    }

    /// 用调用方给出的绑定字节做本机复核：归属关系与执行目录可用性。
    ///
    /// `full` 保留既有调用签名；两种力度现在执行同一套检查（文件对象身份与完整发现
    /// 快照不参与准入）。本机组合的字节来自本机 `session_bindings`，远端组合的字节
    /// 来自远端会话行；两种组合走的是同一套本机判定，因此复核结论不会因为数据在哪
    /// 而不同。
    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace>;

    /// Recheck immutable evidence loaded from the canonical store. The remote adapter
    /// uses this after restart; a missing snapshot must never be reconstructed.
    async fn validate_saved_binding(
        &self,
        binding: &SessionBinding,
        _snapshot: &str,
        _owner: WorkspaceId,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.validate_binding_value(binding, full).await
    }

    /// 本机来源证据是否足以把无绑定历史表达成 legacy（远端组合由门面固定为 `false`）。
    async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool>;

    /// 测试用：本机 SQLite 连接池。
    #[cfg(all(test, not(target_os = "emscripten")))]
    fn sqlite_pool(&self) -> Option<&sqlx::SqlitePool> {
        None
    }
}
