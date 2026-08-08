//! Resources 层访问工厂（M-res 收口：存储实例化点归 Agent 层声明边）。
//!
//! §0 声明边 `Resources --> Agent`（`docs/top-level.md`）：存储具体实现
//! （`SqliteThreadStore` / `FilesystemThreadStore`）位于 peri-resources，
//! peri-agent 经本模块提供实例化工厂，供 ACP 宿主装配面
//! （`host/stdio/init.rs` / `host/assemble.rs`）注入 thread store——
//! ACP 层不直接依赖 Resources。
//!
//! 既有例外（M-res 记录在案）：TUI / print 装配点仍直连
//! `peri_resources::Resources::open_with`（`peri-tui` app/mod.rs /
//! cli_print.rs），不走本工厂；`open_with` / `open_thread_store_with`
//! 双入口固化该不对称。
//!
//! 实例化动作仍经 `peri_resources::Resources` 门面（M-res 验收：
//! 实例化点留在 Resources 层），本模块只做声明边转发。

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::store::ThreadStore;

/// 打开默认 thread 存储并返回共享 `ThreadStore` 句柄。
///
/// 保持 `Resources::open()` 既有行为：默认路径 `~/.peri/threads/threads.db`
/// 打开失败时 fallback 到临时目录。
pub async fn open_thread_store() -> anyhow::Result<Arc<dyn ThreadStore>> {
    open_thread_store_with(None).await
}

/// 按显式路径打开 thread 存储并返回共享 `ThreadStore` 句柄。
///
/// `Some(path)` 直接使用指定 SQLite 路径，打开失败时直接报错
/// （不 fallback 临时目录），错误携带路径；`None` 与 [`open_thread_store`]
/// 行为一致（默认路径 + fallback 临时目录）。
pub async fn open_thread_store_with(
    db_path: Option<PathBuf>,
) -> anyhow::Result<Arc<dyn ThreadStore>> {
    let resources = peri_resources::Resources::open_with(db_path).await?;
    Ok(resources.thread_store())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::open_thread_store_with;

    /// [P1] 显式路径打开成功。
    #[tokio::test]
    async fn test_open_thread_store_with_explicit_path_ok() {
        let dir = tempdir().unwrap();
        open_thread_store_with(Some(dir.path().join("custom/t.db")))
            .await
            .unwrap();
    }

    /// [P1] 显式路径不可用（父级为普通文件）时报错，不 fallback，错误携带路径。
    #[tokio::test]
    async fn test_open_thread_store_with_invalid_path_errs() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "not a directory").unwrap();
        let err = match open_thread_store_with(Some(file.join("t.db"))).await {
            Ok(_) => panic!("父级为普通文件时应返回错误"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains(&file.display().to_string()),
            "错误必须携带路径: {err}"
        );
    }
}
