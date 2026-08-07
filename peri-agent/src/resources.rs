//! Resources 层访问工厂（M-res 收口：存储实例化点归 Agent 层声明边）。
//!
//! §0 声明边 `Resources --> Agent`（`docs/top-level.md`）：存储具体实现
//! （`SqliteThreadStore` / `FilesystemThreadStore`）位于 peri-resources，
//! peri-agent 经本模块提供实例化工厂，供 ACP 宿主装配面
//! （`host/stdio/init.rs` / `host/assemble.rs`）与 TUI 部署装配点注入
//! thread store——ACP / TUI 层不直接依赖 Resources。
//!
//! 实例化动作仍经 `peri_resources::Resources::open()` 门面（M-res 验收：
//! 实例化点留在 Resources 层），本模块只做声明边转发。

use std::sync::Arc;

use peri_acp_types::store::ThreadStore;

/// 打开默认 thread 存储并返回共享 `ThreadStore` 句柄。
///
/// 保持 `Resources::open()` 既有行为：默认路径 `~/.peri/threads/threads.db`
/// 打开失败时 fallback 到临时目录。
pub async fn open_thread_store() -> anyhow::Result<Arc<dyn ThreadStore>> {
    let resources = peri_resources::Resources::open().await?;
    Ok(resources.thread_store())
}
