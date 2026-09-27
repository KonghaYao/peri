//! Resources 层访问工厂（M-res 收口：存储实例化点归 Agent 层声明边）。
//!
//! §0 声明边 `Resources --> Agent`（`docs/top-level.md`）：存储具体实现
//! （`SessionResourcesImpl`）位于 peri-resources，peri-agent 经本模块提供实例化
//! 工厂，供 ACP 宿主装配面（`host/stdio/init.rs` / `host/assemble.rs`）与
//! TUI/print 入口注入会话资源门面——ACP 层不直接依赖 Resources。
//!
//! 门面是消费侧唯一的会话行为句柄：本模块不再提供裸 `ThreadStore` 工厂。实例化
//! 动作仍经 `peri_resources::Resources` 门面（M-res 验收：实例化点留在 Resources
//! 层），本模块只做声明边转发。

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::session_resources::{SessionResources, SessionStoreShutdownPort};
use peri_acp_types::session_store::SessionStoreDeployment;

/// 打开默认会话资源门面的业务句柄（默认路径 `~/.peri/threads/threads.db`）。
///
/// 写打开不可用但历史仍可读时会降级为只读打开（见 `Resources::open_with`）；
/// 失败直接返回错误，不 fallback 临时目录。**不返回部署关闭权**：本入口没有部署
/// 生命周期，连接随句柄释放；需要关闭权的部署入口见 [`open_session_resources_deployment`]。
pub async fn open_session_resources() -> anyhow::Result<Arc<dyn SessionResources>> {
    open_session_resources_with(None).await
}

/// 按部署参数打开会话资源门面（D-04 各部署入口的唯一入口），返回**业务句柄 +
/// 部署关闭权**。
///
/// 定位、凭证来源与访问意图都是 [`SessionStoreDeployment`] 的中性事实：本函数不解释
/// locator、不读凭证值、不选择后端——解析与后端选择只发生在 `peri_resources::Resources`
/// 门面。打开失败按原错误上抛（含配置错误、只读失败分类与远程未接线），不 fallback。
///
/// 关闭权（`Box<dyn SessionStoreShutdownPort>`，non-Clone）只属于部署：调用方（stdio
/// 宿主等）把业务句柄注入业务侧，把关闭权留在宿主配置里，在任务排空之后关闭存储。
pub async fn open_session_resources_deployment(
    deployment: &SessionStoreDeployment,
) -> anyhow::Result<(Arc<dyn SessionResources>, Box<dyn SessionStoreShutdownPort>)> {
    let resources = peri_resources::Resources::open_deployment(deployment).await?;
    let (business, shutdown) = resources.into_parts();
    Ok((business, Box::new(shutdown)))
}

/// 既有 `--db-path` 兼容入口：归一为显式本机路径，不在这里解释后端。
///
/// `Some(path)` 直接使用指定 SQLite 路径，打开失败时直接报错（不 fallback 临时
/// 目录），错误携带路径；`None` 与 [`open_session_resources`] 行为一致。只交业务
/// 句柄（装配测试用），不交关闭权。
pub async fn open_session_resources_with(
    db_path: Option<PathBuf>,
) -> anyhow::Result<Arc<dyn SessionResources>> {
    let resources = peri_resources::Resources::open_with(db_path).await?;
    Ok(resources.into_session_resources())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::open_session_resources_with;

    /// [P1] 显式路径打开成功。
    #[tokio::test]
    async fn test_open_session_resources_with_explicit_path_ok() {
        let dir = tempdir().unwrap();
        open_session_resources_with(Some(dir.path().join("custom/t.db")))
            .await
            .unwrap();
    }

    /// [P1] 显式路径不可用（父级为普通文件）时报错，不 fallback，错误携带路径。
    #[tokio::test]
    async fn test_open_session_resources_with_invalid_path_errs() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "not a directory").unwrap();
        let err = match open_session_resources_with(Some(file.join("t.db"))).await {
            Ok(_) => panic!("父级为普通文件时应返回错误"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains(&file.display().to_string()),
            "错误必须携带路径: {err}"
        );
    }
}
