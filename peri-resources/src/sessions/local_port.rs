//! 工作区发现与绑定校验端口；不管理会话执行所有权。

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, WorkspaceId};

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

    /// 用调用方给出的绑定字节做本机复核：目录关系、关键文件对象，`full` 为真时再叠一次
    /// 完整发现快照比对（一次准入的权威复核）。
    ///
    /// 本机组合的字节来自本机 `session_bindings`，远端组合的字节来自远端会话行；两种组合
    /// 走的是同一套本机判定，因此复核结论不会因为数据在哪而不同。
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
