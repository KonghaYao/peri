//! MCP core：stdio 与 Streamable HTTP 共用的协议实现（WP-005）。
//!
//! 模块划分与不变量：
//!
//! | 子模块 | 唯一职责 |
//! | --- | --- |
//! | [`catalog`] | 冻结夹具 → 七工具 `tools/list` 条目与源事实元数据 |
//! | [`error_map`] | [`crate::error::ToolError`] → JSON-RPC 错误；[`crate::wire::ToolResponse`] → tool result |
//! | [`identity`] | 可信 `(principal, client_instance)` 与请求上下文构造 |
//! | [`resources`] | `sandbox://tasks` 资源面、URI 校验、订阅轮询 |
//! | [`server`] | `ServerHandler`：七工具注册、legacy/modern 生命周期、资源与订阅 |
//!
//! 三条不变量（已在 `server_test.rs` 中用静态断言钉住）：
//!
//! 1. **协议层不做工具语义**：本模块只依赖注入的 [`crate::wire::ToolExecutor`]，
//!    不含任何文件系统或进程访问。单进程形态（D-003）下这条依然成立且更有意义：
//!    工具在**同一个进程**里执行，所以"协议层不碰文件系统与进程"只能靠结构保证，
//!    而不是靠进程边界——唯一的执行入口就是那个注入的执行器。
//! 2. **工具面恰好七个**：条目逐字来自冻结夹具，别名只参与 `tools/call` 名称解析，
//!    Bash 输入字段不增不减。
//! 3. **能力声明 = 真实可达面**：没有注入任务来源时就不声明 `resources`，
//!    不允许"声明了但请求不可达"。

pub mod catalog;
pub mod error_map;
pub mod identity;
pub mod resources;
pub mod server;

pub use catalog::{metadata, metadata_for, tools, Catalog, ToolMetadata};
pub use identity::ConnectionIdentity;
pub use resources::{parse_task_uri, task_uri, TaskResourceUri, TaskStatusSource};
pub use server::{SandboxServer, SUPPORTED_VERSIONS};
