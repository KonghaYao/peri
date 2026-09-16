//! # local-mcp-server
//!
//! 把 Peri 的七个工具（`Read`/`Write`/`Edit`/`Glob`/`Grep`/`folder_operations`/`Bash`）
//! 迁移成**独立**的 MCP server：**单进程**直接在本机执行，支持 stdio 与 Streamable
//! HTTP 两条传输，遵循 MCP `2026-07-28`（兼容 `2025-11-25` legacy 握手）。
//!
//! ## 执行形态与边界（D-003）
//!
//! - 没有容器、没有镜像、没有 worker 子进程：七工具在本进程内执行。
//! - 文件类工具（Read/Write/Edit/Glob/Grep/folder_operations）一律限定在启动参数
//!   指定的**工作区根**内（canonicalize + 越界与符号链接防护）。
//! - `Bash` 以当前用户权限执行、以工作区根为 cwd，命令本身不额外限制。
//! - 工作区根是**能力边界，不是安全边界**：它约束文件类工具的路径解析，
//!   不对进程的读写、网络或进程能力提供隔离保证。
//!
//! ## 模块所有权
//!
//! | 模块 | 唯一职责 | 实现 owner |
//! | --- | --- | --- |
//! | [`config`] | 启动配置（传输/暴露面/授权来源/工作区/任务保留） | WP-P1 |
//! | [`error`] | 共享错误类型与规范错误码 | WP-P1 |
//! | [`wire`] | 共享 DTO（MCP 面）与执行缝 trait | WP-P1 |
//! | [`capability`] | 路径授权与边界判定 | WP-P3 |
//! | [`tools::fs`] | Read/Write/Edit/Glob/folder_operations 语义 | WP-P3 |
//! | [`output`] | UTF-8 安全截断与落盘路径 | WP-P3 |
//! | [`tools::grep`] | Grep 别名/模式/输出语义 | WP-P2 |
//! | [`tools::bash`] | Bash 三字段解析与结果投影 | WP-P2 |
//! | [`tasks`] | 唯一任务注册表与 Bash 进程执行器 | WP-P2 |
//! | [`runtime`] | 进程内执行内核（七工具分派） | WP-P2 |
//! | [`mcp`] | MCP core：`ServerHandler`、七工具注册 | WP-P4 |
//! | [`protocol`] | legacy/modern 生命周期、结果与错误映射、resources | WP-P4 |
//! | [`transport::stdio`] | stdio framing 与关闭语义 | WP-P4 |
//! | [`transport::http`] | Streamable HTTP 与暴露面控制 | WP-P4 |
//! | [`auth`] | token 校验与 principal/连接实例绑定 | WP-P4 |
//! | [`transport`] | 传输枚举与共享传输契约 | WP-P4 |
//! | [`observe`] | 日志出口、脱敏与审计 | WP-P4 |
//!
//! ## 生产路径（单进程执行内核）
//!
//! ```text
//! MCP core → runtime::InProcessExecutor ─┬─ tools::fs::FsRuntime   （五工具）
//!                                        ├─ tools::grep::invoke     （Grep）
//!                                        └─ tasks::TaskRegistry     （Bash：唯一任务表）
//! ```
//!
//! 调用链上没有帧编码/解码、管道、请求配对、超时兜底或进程监督；两条传输
//! （stdio/HTTP）与 `src/main.rs` 的装配共用**同一个**执行器，因此不存在
//! "某个工具只在某条传输上可用"的实现分支。
//!
//! ## 规范基线
//!
//! 协议侧以 MCP `2026-07-28`（final）为准，同时保留 `2025-11-25` legacy 握手；
//! 条款到 wire 断言的映射见 `artifacts/designs/WP-001/protocol-baseline.md`，
//! 接口冻结见 `artifacts/designs/WP-001/interfaces.md`。

pub mod auth;
pub mod capability;
pub mod config;
pub mod error;
pub mod mcp;
pub mod observe;
pub mod output;
pub mod protocol;
pub mod runtime;
pub mod tasks;
pub mod tools;
pub mod transport;
pub mod wire;

pub use config::Config;
pub use error::{CapabilityError, ConfigError, ErrorPayload, ToolError, WorkerError};
pub use wire::{
    ClientInstanceId, PrincipalId, RequestContext, RequestId, StructuredOutput, TaskHandle, TaskId,
    TaskSnapshot, TaskStatus, ToolExecutor, ToolRequest, ToolResponse,
};
