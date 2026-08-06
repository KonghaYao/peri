//! Resources context — 外部系统访问通道的唯一实例化入口。
//!
//! 本迁移点先落 context 形状 + 唯一实例化入口（TUI 启动处）；
//! Controller/Runtime 建成后消费方随 L2/L3/L5 跟进接入（属预期过渡态，
//! 接口按目标态设计，避免二次返工）。

use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::store::ThreadStore;

use crate::sessions::SqliteThreadStore;

/// 外部系统资源门面
#[derive(Clone)]
pub struct Resources {
    thread_store: Arc<dyn ThreadStore>,
}

impl Resources {
    /// 打开全部资源（当前为会话存储）。
    ///
    /// 保持既有行为：默认路径 `~/.peri/threads/threads.db` 打开失败时
    /// fallback 到临时目录。
    pub async fn open() -> Result<Self> {
        let store = match SqliteThreadStore::default_path().await {
            Ok(store) => store,
            Err(_) => SqliteThreadStore::new(std::env::temp_dir().join("zen-threads.db"))
                .await
                .map_err(|e| anyhow::anyhow!("无法创建临时 SQLite 数据库: {e}"))?,
        };
        Ok(Self {
            thread_store: Arc::new(store),
        })
    }

    /// 会话存储句柄（trait object，供 Agent/ACP/TUI 注入）
    pub fn thread_store(&self) -> Arc<dyn ThreadStore> {
        self.thread_store.clone()
    }
}
