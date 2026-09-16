//! 任务层：Bash 进程生命周期、owner 绑定状态、日志与 TTL 回收。
//!
//! 单进程形态（D-003）下只有**一份**任务事实：
//!
//! | 类型 | 职责 |
//! | --- | --- |
//! | [`bash::BashTasks`] | spawn `bash -c`、进程组、日志 tee、TERM→KILL（唯一持有 `Child`） |
//! | [`registry::TaskRegistry`] | **唯一**任务表：铸造 id、`(principal, client_instance)` 绑定、容量、TTL、事件、资源快照 |
//!
//! 注册表直接持有 [`bash::BashTasks`] 返回的任务句柄（`Arc<BashTask>`），因此
//! 状态查询、日志读取与停止都是进程内调用：没有 RPC 通道、没有第二张表、没有
//! 轮询式状态同步（轮询只读进程内状态，用于收敛 TTL 基准与事件）。
//!
//! ## MCP 可访问的控制面（不新增 `Bash` 输入字段）
//!
//! 1. **标准资源**：`sandbox://tasks`（列表）与 `sandbox://tasks/{task_id}`（单任务），
//!    载荷是冻结的 [`crate::wire::TaskSnapshot`]；状态变化通过
//!    [`registry::TaskRegistry::subscribe`] 事件流交给协议层发布通知。
//! 2. **普通 Bash 控制**：返回文本保留源的 `kill {pid}` / `kill -- -{pgid}` 指引；
//!    任务注册表另提供 [`registry::TaskRegistry::stop`]（同一进程组 TERM→KILL），
//!    两条路径操作的是同一个进程组，不存在"只有内部 API 才能停止"的任务。
//! 3. **日志**：日志文件路径随 handle 返回，普通 `Read` 可读（日志目录在工作区
//!    根内，因此 `Read` 的 capability 判定对它同样成立）；
//!    机器可读读取走 [`registry::TaskRegistry::read_log`]/`read_output`。

pub mod bash;
pub mod log;
pub mod params;
pub mod registry;

pub use bash::{
    BashTaskConfig, BashTaskError, BashTasks, KILL_ESCALATION, MAX_PARTIAL_CAPTURE_BYTES,
    SHUTDOWN_GRACE,
};
pub use log::{
    is_valid_log_handle, mint_log_handle, DirOutputPersist, LogChunk, LogStore, LogStream,
    OutputPersist, RootedDir,
};
pub use params::{BashMode, BashStartPayload, BashTaskState, TaskLogPayload, MAX_LOG_READ_BYTES};
pub use registry::{
    mint_task_id, summarize, task_resource_uri, BashRun, BashRunArgs, BashRunError, Clock,
    SystemClock, TaskAccessError, TaskEvent, TaskEventKind, TaskOutput, TaskRegistry,
    TaskRegistryConfig, TASKS_RESOURCE_URI,
};
