//! Goal 能力域：Goal 工具与 steering 中间件

pub mod middleware;
pub mod tool;

pub use middleware::GoalMiddleware;
pub use tool::GoalTool;
