//! 工作区注册表（独立于 chat 的上层概念）。
//!
//! workspace = 本地目录（cwd）定义：其下新建的对话继承该 cwd（ACP 进程
//! 工作目录 + session/list 查询面）。内存表是进程内权威（create 时同步
//! 投影到 Registry Doc `workspaces` map，跨重启可见）；启动时经
//! [`WorkspaceRegistry::rebuild`] 从 Registry Doc 重建内存表（保证重启后
//! `chat/create` 携带 workspace_id 仍能解析 cwd）。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use acp_hub_proto::schema::WorkspaceSummary;

use crate::protocol::validate_cwd;
use crate::state::registry::{RegistryError, RegistryState};

/// 工作区条目（进程内镜像）。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRecord {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkspaceRecord {
    fn to_summary(&self) -> WorkspaceSummary {
        WorkspaceSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            created_at: self.created_at.to_rfc3339(),
            updated_at: self.updated_at.to_rfc3339(),
        }
    }

    fn from_summary(s: &WorkspaceSummary) -> Self {
        WorkspaceRecord {
            id: s.id.clone(),
            name: s.name.clone(),
            cwd: s.cwd.clone(),
            created_at: s
                .created_at
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            updated_at: s
                .updated_at
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        }
    }
}

/// 工作区操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    /// 目标不存在（remove/get 或 create 引用已删除工作区）。
    #[error("workspace not found: {0}")]
    NotFound(String),
    /// cwd 形态非法（非绝对路径/NUL/控制字符/超长，§4.3 裁决同款）。
    #[error("invalid cwd: {0}")]
    CwdInvalid(String),
    /// cwd 目录不存在（与 instance spawn cwd 校验一致）。
    #[error("cwd not found: {0}")]
    CwdMissing(String),
    /// Registry 投影写失败。
    #[error("registry write failed: {0}")]
    Registry(#[from] RegistryError),
}

/// 工作区注册表（内存权威 + Registry Doc 投影）。
#[derive(Clone)]
pub struct WorkspaceRegistry {
    inner: Arc<WorkspaceInner>,
}

struct WorkspaceInner {
    workspaces: RwLock<HashMap<String, WorkspaceRecord>>,
    registry: RegistryState,
}

impl WorkspaceRegistry {
    /// 以 Registry 状态源句柄构建。
    pub fn new(registry: RegistryState) -> Self {
        WorkspaceRegistry {
            inner: Arc::new(WorkspaceInner {
                workspaces: RwLock::new(HashMap::new()),
                registry,
            }),
        }
    }

    /// Registry 状态源句柄。
    pub fn registry(&self) -> RegistryState {
        self.inner.registry.clone()
    }

    /// 创建工作区：校验 cwd（形态 + 目录存在性）→ 生成 id → 内存登记 →
    /// 投影 Registry Doc `workspaces` map。
    pub async fn create(&self, name: &str, cwd: &str) -> Result<WorkspaceRecord, WorkspaceError> {
        validate_cwd(cwd).map_err(|e| WorkspaceError::CwdInvalid(e.to_string()))?;
        if !std::path::Path::new(cwd).is_dir() {
            return Err(WorkspaceError::CwdMissing(cwd.to_string()));
        }
        let id = Uuid::new_v4().to_string();
        // 名称缺省：以目录名兜底（与 chat 标题缺省同风格）。
        let name = if name.trim().is_empty() {
            std::path::Path::new(cwd)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| id[..8].to_string())
        } else {
            name.trim().to_string()
        };
        let now = Utc::now();
        let rec = WorkspaceRecord {
            id,
            name,
            cwd: cwd.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.inner
            .workspaces
            .write()
            .await
            .insert(rec.id.clone(), rec.clone());
        if let Err(e) = self.inner.registry.upsert_workspace(rec.to_summary()).await {
            warn!(workspace_id = %rec.id, error = ?e, "workspace registry projection failed");
            return Err(e.into());
        }
        Ok(rec)
    }

    /// 删除工作区定义（不影响已建对话/会话；仅移除 Registry Doc 条目）。
    pub async fn remove(&self, id: &str) -> Result<(), WorkspaceError> {
        let mut ws = self.inner.workspaces.write().await;
        if ws.remove(id).is_none() {
            return Err(WorkspaceError::NotFound(id.to_string()));
        }
        drop(ws);
        if let Err(e) = self.inner.registry.remove_workspace(id).await {
            warn!(workspace_id = id, error = ?e, "workspace registry remove failed");
            return Err(e.into());
        }
        Ok(())
    }

    /// 查询（create 时解析 cwd）。
    pub async fn get(&self, id: &str) -> Option<WorkspaceRecord> {
        self.inner.workspaces.read().await.get(id).cloned()
    }

    /// 全量列表（UI 展示；registry 投影已覆盖，此为主要查询面）。
    pub async fn list(&self) -> Vec<WorkspaceRecord> {
        let ws = self.inner.workspaces.read().await;
        let mut out: Vec<_> = ws.values().cloned().collect();
        out.sort_by_key(|a| a.created_at);
        out
    }

    /// 启动恢复：从 Registry Doc 重建内存表（hub 装配时调用一次；registry
    /// doc 已从 update_log 重放）。失败仅告警——内存表为空时 create 引用
    /// workspace_id 会报 NotFound，UI 可重新定义。
    pub async fn rebuild(&self) {
        match self.inner.registry.list_workspaces().await {
            Ok(list) => {
                let mut ws = self.inner.workspaces.write().await;
                for s in &list {
                    ws.insert(s.id.clone(), WorkspaceRecord::from_summary(s));
                }
                if !list.is_empty() {
                    tracing::info!(count = list.len(), "workspace registry rebuilt from doc");
                }
            }
            Err(e) => warn!(error = ?e, "workspace rebuild failed (registry read)"),
        }
    }
}

#[cfg(test)]
#[path = "workspace_registry_test.rs"]
mod workspace_registry_test;
