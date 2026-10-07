use std::sync::Arc;

mod claim;
mod cold;
mod context;
mod delegation;
pub use cold::{
    ChildIdentityResolution, ChildResumeMetadata, ColdChildBlocked, ColdChildExecution,
    ColdChildRuntime,
};
mod resume;
mod spawn;

#[cfg(test)]
#[path = "factory/completion_delivery_test.rs"]
mod completion_delivery_tests;

use super::types::{SubagentResumeConfig, SubagentSpawnConfig, SubagentSpawned};
use crate::session::Session;
pub(super) use claim::{clear_stopped_attempt, ResumeClaim};
use resume::resume_subagent_impl;
use spawn::spawn_subagent_impl;

fn completion_delivery(
    parent: Option<&Arc<Session>>,
    configured: Option<peri_acp_types::tasks::OnBgCompleteFn>,
) -> Option<peri_acp_types::tasks::OnBgCompleteFn> {
    configured.or_else(|| {
        parent.map(|parent| {
            let router = crate::session::async_router::AsyncRouter::for_queue(parent.queue());
            Arc::new(
                move |result: &peri_acp_types::event::BackgroundTaskResult, kind| {
                    router.route_bg_result(result, kind);
                    Ok(())
                },
            ) as peri_acp_types::tasks::OnBgCompleteFn
        })
    })
}

/// resume 的 thread id 格式校验：非 UUID 必须在**存在性判定之前**报错。
///
/// 与 `ResumeClaim` 的 `validate_thread` 共用同一判据：两条报错路径（快照读取的
/// `NotFound` 与认领前的存在性校验）不能把「格式非法」说成「不存在」。
pub(super) fn validate_thread_id_format(
    thread_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if uuid::Uuid::parse_str(thread_id).is_err() {
        return Err(format!("resume_subagent: invalid thread id: {}", thread_id).into());
    }
    Ok(())
}

/// 沿 parent 链解析会话树的根（child 保存与 resume 归属校验共用）。
///
/// 与旧实现的语义一致：显式深度上限与环检测，两者任一触发即拒绝——不能靠
/// 「走到没有 parent 为止」把环形数据当成合法层级。
pub(super) async fn execution_root(
    store: &dyn peri_acp_types::session_resources::SessionResources,
    id: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut current = id.to_owned();
    let mut visited = std::collections::HashSet::new();
    loop {
        if visited.len() >= 128 || !visited.insert(current.clone()) {
            return Err(peri_acp_types::workspace::WorkspaceError::InvalidBinding.into());
        }
        match store
            .load_session_meta(&current)
            .await
            .map_err(|error| format!("session {current} is not readable: {error}"))?
            .parent_thread_id
        {
            Some(parent) => current = parent,
            None => return Ok(current),
        }
    }
}

// ─── 统一入口 ────────────────────────────────────────────────────────────────

/// Agent 层 session 工厂（L3）：subagent 创建统一入口命名空间。
///
/// 验收契约（子 issue L3）：`SessionFactory::spawn_subagent(parent, config)`
/// 为唯一 subagent 创建入口，位于 peri-agent。Middleware 只组装
/// [`SubagentSpawnConfig`] 发起意图，不持有创建实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionFactory;

impl SessionFactory {
    /// 启动子 agent（唯一创建入口，见 [`spawn_subagent_impl`] 的流程说明）。
    pub async fn spawn_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        spawn_subagent_impl(parent, config).await
    }

    /// 恢复子 agent（唯一恢复入口，见 [`resume_subagent_impl`] 的校验流程说明）。
    ///
    /// 主 agent 凭中断/错误/bg 通知文本携带的 `child_thread_id` 重新唤起被中断的
    /// subagent：从磁盘 thread_store 加载 meta 校验（存在 / 非 active）后重建现场
    /// 继续执行。thread_id 不变，可无限次恢复。
    pub async fn resume_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        resume_subagent_impl(parent, config).await
    }
}
