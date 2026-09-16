//! 协议契约：legacy/modern 生命周期、任务资源与错误边界（WP-005）。
//!
//! 本模块是 MCP 协议层**可复用的判定逻辑**，被 [`crate::mcp::server`]（两条传输共用的
//! `ServerHandler`）消费；[`crate::transport::stdio`] 与 [`crate::transport::http`] 只负责
//! 选帧、暴露面与关闭语义，不得自行复制任何一条下面的判定。
//!
//! ## 1. 两代并存（dual-era）
//!
//! | 事实 | 依据 | 代码 | wire 证据 |
//! | --- | --- | --- | --- |
//! | era 由**请求**决定，不看连接状态 | `basic/versioning` | [`era::Era::of`] | `tests/transport_stdio.rs::test_stdio_modern_lifecycle_direct_requests_and_version_negotiation` |
//! | 不支持版本 → `-32022` 且带 `data.supported`/`data.requested` | `basic/versioning#protocol-version-negotiation` | [`crate::mcp::server::SUPPORTED_VERSIONS`] | 同上 |
//! | `server/discover` 必须实现且不依赖 `initialize` | `server/discover` | `SandboxServer::discover` | 同上 |
//! | legacy 走 `initialize`→`initialized`，modern 可直接发请求 | `basic/versioning#backward-compatibility...` | [`era`] | `test_stdio_legacy_handshake_lists_exactly_seven_frozen_tools`、`test_stdio_direct_modern_call_needs_no_handshake` |
//! | modern 结果带 `resultType`，legacy 不带 | `basic/index#responses` | `error_map::call_tool_result` | 上述两条用例 |
//!
//! 判定只有一个实现：[`era`]。它是纯函数（输入是版本字符串），因此"modern 无会话"是结构
//! 事实：该函数没有会话状态的输入可读。
//!
//! ## 2. 请求方法归因
//!
//! SDK 无法把请求解析成强类型结构时会退化成 `CustomRequest`（方法名保留、形状已丢失）。
//! [`methods::classify_unrouted`] 把这种请求分成"方法不存在"（`-32601`）与"参数形状不合法"
//! （`-32602`），两者的边界由 `artifacts/designs/WP-001/interfaces.md` §4.1 冻结。
//!
//! ## 3. 任务资源与订阅（不新增工具输入字段）
//!
//! Bash 的输入字段永远是 `command`/`timeout`/`run_in_background`；任务状态经**标准 MCP
//! 资源**暴露，而不是靠给工具加参数（`server/tools#stateful-tools`）：
//!
//! | 面 | 形状 | 身份约束 |
//! | --- | --- | --- |
//! | 集合 | `sandbox://tasks` | 只列出调用方自己的任务 |
//! | 单任务 | `sandbox://tasks/{task_id}` | owner 与连接实例都必须匹配 |
//! | legacy 订阅 | `resources/subscribe` → `notifications/resources/updated` | 订阅前与每次通知前都再次判定 owner |
//! | modern 订阅 | `subscriptions/listen` → ack（带 `subscriptionId`）→ 同上通知 | 同上；服务端下线订阅流时发 `notifications/cancelled` |
//!
//! "他人的任务"与"不存在的任务"必须同形（`-32602`，`data.uri` 回显请求值），否则资源面
//! 会成为任务存在性的探针。旧码 `-32002` 在 `2026-07-28` 起**不得**发出。
//!
//! ## 4. 错误边界（唯一映射入口）
//!
//! | 情形 | 形态 | 实现 |
//! | --- | --- | --- |
//! | 未知工具名 | JSON-RPC `-32602` + `Unknown tool: <name>` | [`crate::error::ToolError::UnknownTool`] |
//! | 请求形状不合法 | JSON-RPC `-32602` | [`methods`] + [`crate::mcp::error_map`] |
//! | 内部失败 / 后端不可用 | JSON-RPC `-32603`（fail closed，不降级） | [`crate::error::ToolError`] |
//! | 工具业务失败 | tool result 且 `isError: true`（SEP-1303） | [`crate::mcp::error_map::call_tool_result`] |
//! | 资源不存在 / 越权 | JSON-RPC `-32602` + `data.uri` | [`crate::mcp::server`] |
//! | 无效分页游标 | JSON-RPC `-32602` | 见 §5 |
//!
//! ## 5. 分页与进度：明确不适用，而不是静默省略
//!
//! - **分页**（`server/utilities/pagination`）：本服务的列表面是单页——`tools/list` 固定七项，
//!   `resources/list` 只列调用方自己的任务。服务端**从不**发出 `nextCursor`；因此任何调用方
//!   提交的 `cursor` 都不是本服务发出的游标，按规范 "Invalid cursors SHOULD result in an
//!   error with code -32602 (Invalid params)" 拒绝（`test_stdio_pagination_cursors_are_rejected`）。
//! - **进度**（`basic/patterns/progress`）：规范允许 "Servers receiving a request with a
//!   progress token MAY choose not to send any progress notifications"，本服务选择不发，
//!   且 `progressToken` 不参与授权或业务判定
//!   （`test_stdio_client_state_and_progress_token_never_influence_execution`）。
//! - **MRTR**（`basic/patterns/mrtr`）：本服务不发出 `input_required`（规范为 MAY），因此
//!   不需要 `requestState` 完整性保护；请求里出现的 `requestState`/`inputResponses` 一律被
//!   当作攻击者可控输入忽略，绝不透传给执行层。

pub mod era;
pub mod methods;

pub use era::{era_of, is_modern, Era};
pub use methods::{classify_unrouted, is_implemented, UnroutedRequest, IMPLEMENTED_METHODS};
