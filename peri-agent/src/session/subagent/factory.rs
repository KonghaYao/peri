use std::sync::Arc;

mod claim;
mod context;
mod resume;
mod spawn;

use super::types::{SubagentResumeConfig, SubagentSpawnConfig, SubagentSpawned};
use crate::session::Session;
pub(super) use claim::ResumeClaim;
use resume::resume_subagent_impl;
use spawn::spawn_subagent_impl;

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
