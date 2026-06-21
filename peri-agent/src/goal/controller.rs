//! GoalController — goal 读写接口（供 Goal 工具和 GoalMiddleware 依赖注入）。
//!
//! 定义在 peri-agent 层避免 peri-middlewares → peri-acp 循环依赖。
//! peri-acp 的 GoalState 实现此 trait。

use async_trait::async_trait;

use super::model::GoalStatus;
use super::view::GoalViewSnapshot;

/// Goal 读写控制器接口
#[async_trait]
pub trait GoalController: Send + Sync {
    /// 创建 goal。如果 goal 已存在返回 Err。
    async fn create_goal(&self, objective: String) -> Result<(), String>;

    /// 声明完成。状态转换非法时返回 Err。
    async fn complete_goal(&self) -> Result<(), String>;

    /// 声明阻塞。reason 必填。状态转换非法时返回 Err。
    async fn block_goal(&self, reason: String) -> Result<(), String>;

    /// 只读快照（get action + after_agent 判断用）
    fn snapshot(&self) -> GoalViewSnapshot;
}

/// GoalController 的补充视图（after_agent 只需判断 active）
pub fn is_active(snap: &GoalViewSnapshot) -> bool {
    snap.status == Some(GoalStatus::Active)
}
