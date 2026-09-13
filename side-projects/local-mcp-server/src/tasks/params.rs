//! Bash 任务的进程内参数与状态结构。
//!
//! 单进程形态（D-003）下没有 RPC 通道：这些结构是**同一进程内的普通 Rust 值**，
//! 由 [`crate::tasks::registry::TaskRegistry`] 构造并交给
//! [`crate::tasks::bash::BashTasks`]。它们**不是** MCP 通道载荷，也不改变 `Bash`
//! 的公开输入 schema（三字段 `command`/`timeout`/`run_in_background` 由
//! [`crate::tools::bash`] 解析）。
//!
//! 只有**仍在进程内传递**的结构留在这里：启动参数（[`BashStartPayload`]）、
//! 状态快照（[`BashTaskState`]）与日志读取结果（[`TaskLogPayload`]）。
//! 容器期按 id 查询/停止/读日志的 RPC 载荷已随通道一并删除——注册表直接持有
//! 任务句柄，不需要"猜 id + 句柄校验"的跨进程防御面。
//!
//! 三个不变量：
//!
//! 1. `task_id` 由注册表单点铸造（`shell-<UUIDv7 完整>`），任务层只接受该 id，
//!    不自造 id。
//! 2. `log_handle` 是高熵不透明句柄（[`crate::tasks::log::mint_log_handle`]）；
//!    按 id 操作任务时必须回带它，句柄不匹配即拒绝，因此猜到 task id 也无法
//!    操作或读取他人任务。
//! 3. 状态快照（[`BashTaskState`]）由任务自身生成，注册表只投影为对外的
//!    [`crate::wire::TaskSnapshot`]；不存在第二份可独立漂移的任务状态。

use serde::{Deserialize, Serialize};

use crate::wire::{TaskId, TaskStatus};

/// 单次日志读取的字节上限（防止把日志整体搬进模型上下文）。
pub const MAX_LOG_READ_BYTES: usize = 65_536;

/// Bash 启动模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashMode {
    /// 前台：本次调用等待到超时或结束；超时后进程**继续存活**（提升为后台）。
    Foreground,
    /// 显式后台：立即返回；`timeout_ms` 到期时终止进程组。
    Background,
}

/// 启动一次 Bash 任务的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashStartPayload {
    /// 注册表铸造的任务 id。
    pub task_id: TaskId,
    /// 高熵日志句柄（日志文件命名与后续读取的唯一键）。
    pub log_handle: String,
    /// 原始命令（`bash -c` 脚本语义，不做转义）。
    pub command: String,
    /// 工作目录（宿主工作区根）。
    pub cwd: String,
    /// 前台/后台模式。
    pub mode: BashMode,
    /// 终止期限（毫秒）：前台到期提升为后台任务（不终止）；后台到期终止进程组。
    pub timeout_ms: Option<u64>,
    /// 本次等待上限（毫秒）：`None` = 不等待，立即返回当前状态。
    pub await_ms: Option<u64>,
}

/// 日志读取结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLogPayload {
    /// UTF-8 安全的日志尾部内容。
    pub content: String,
    /// 该流日志总字节数。
    pub total_bytes: u64,
    /// 是否只返回了尾部。
    pub truncated: bool,
}

/// 任务状态快照（注册表映射为 [`crate::wire::TaskSnapshot`] 并补充 owner）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashTaskState {
    /// 任务 id。
    pub task_id: TaskId,
    /// 生命周期状态。
    pub status: TaskStatus,
    /// 进程 id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 进程组 id（`kill -- -<pgid>`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// stdout 日志路径（宿主工作区根内的私有日志目录）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    /// stderr 日志路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
    /// 退出码（终态）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 采集到的 stdout（最多 `MAX_PARTIAL_CAPTURE_BYTES`）。
    pub stdout: String,
    /// 采集到的 stderr（同上）。
    pub stderr: String,
    /// 是否由前台超时提升为后台任务。
    pub promoted: bool,
    /// 是否因超时或取消被终止。
    pub timed_out: bool,
    /// 是否由协议层取消（`notifications/cancelled` 或连接关闭）触发终止。
    pub cancelled: bool,
    /// 已运行毫秒数。
    pub elapsed_ms: u64,
    /// 启动时间（RFC 3339）。
    pub started_at: String,
    /// 结束时间（RFC 3339）；运行中为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// 结束摘要文本（终态：合并 + 截断后的输出；运行中为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_output: Option<String>,
    /// 终态输出是否触发工具内部限额（2000 行 / 65000 字节）。
    pub truncated: bool,
    /// 触发限额时全量输出的落盘路径（工作区根内的私有产物目录）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_path: Option<String>,
    /// 前台超时提升时的进程状态快照（`ps -o pid=,stat=,etime=,command=`，尽力而为）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_state: Option<String>,
}

impl BashTaskState {
    /// 是否为终态。
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}
