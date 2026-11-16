//! 后台任务显示区域组件。
//!
//! 位于 AppShell 根层 StatusBar 下方，每行展示一个活跃的 bg subagent / bg shell / workflow 任务。
//! 格式：`● coder  修改文档  2m15s`，空态高度 0。

mod status_symbol;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod bg_task_area;
pub use bg_task_area::*;
