//! 进程内执行内核（D-003：单进程、直接在本机执行）。
//!
//! 本模块是七工具在生产路径上的**唯一**执行入口：MCP core 只依赖
//! [`crate::wire::ToolExecutor`]，其生产实现是 [`InProcessExecutor`]。
//!
//! ```text
//! MCP core → runtime::InProcessExecutor ─┬─ tools::fs::FsRuntime   （Read/Write/Edit/Glob/folder_operations）
//!                                        ├─ tools::grep::invoke     （Grep）
//!                                        └─ tasks::TaskRegistry     （Bash：唯一任务表 + 进程句柄）
//! ```
//!
//! 调用链上没有任何帧编码/解码、管道、请求配对、超时兜底、poison/abandoned 记录
//! 或 worker 进程监督——容器期的 RPC 往返与双份任务表已随 D-003 一并移除。
//!
//! ## 与 MCP 协议面的边界
//!
//! 本模块**不**参与协议协商、身份校验与传输：那些在 `mcp/**`、`protocol/**`、
//! `transport/**`、`auth/**`。执行器只回答一个问题——"这一次工具调用在本进程内
//! 该调用哪个语义实现，结果如何投影"。

pub mod executor;

pub use executor::InProcessExecutor;

/// 本产品私有日志/产物目录（相对工作区根）：`.local-mcp`。
///
/// 刻意放在**工作区根内**而不是系统临时目录：Bash 返回文本会指引调用方用 `Read`
/// 查看日志，而 `Read` 的 capability 根就是工作区——日志落在根外就等于指引失效。
pub const PRIVATE_DIR: &str = ".local-mcp";

/// 本产品私有日志目录（相对工作区根）：`<workspace>/.local-mcp/logs`。
///
/// 与 [`crate::output::DEFAULT_ARTIFACT_DIR`]（`.local-mcp/artifacts`）同属本产品
/// 自产文件，两者都经 capability 校验后写入。
pub const LOG_SUBDIR: &str = ".local-mcp/logs";
