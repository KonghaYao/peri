# P1：MCP 取消 RPC 无期限阻塞 ACP 控制面

**状态**：已修复，待 PR 合并。
**优先级**：P1。
**类型**：错误/并发。
**创建日期**：2026-10-24。
**来源**：代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 e598d178）。

## 确认证据

以下行号均以基线 `e598d178` 为准。

**观察（代码路径）**

1. `ServerLoop::run` 对未特判请求（包含 `session/cancel-bg-task`）直接 inline 调用 `dispatch_request`：`peri-acp/src/host/server_loop.rs:52`。
2. `dispatch_request` 兜底分支在 await handler 期间持有全局 `sessions` guard：`peri-acp/src/host/server_loop.rs:606-616`。持有期间，其他需要该锁的请求无法推进。
3. `handle_cancel_bg_task` 只从 `cfg.session_manager` 取 `task_manager`，随后 `cancel_async(task_id).await`：`peri-acp/src/host/requests/session_tasks.rs:25-44`。
4. 外部任务分支 await 注册时保存的 cancel 闭包：`peri-agent/src/agent/async_tasks/manager.rs:662-669`（该闭包本身没有期限）。
5. MCP 外部任务闭包调用 `peer.cancel_task(params).await`：`peri-middlewares/src/mcp/client/subscription_task_binding.rs:197-201`。
6. rmcp 3.5.1 的 `cancel_task` 走 `send_request`（`PeerRequestOptions::no_options()`，timeout=None）：`rmcp-3.5.1/src/service/client.rs:1432-1442`（补丁后源码）；同一版本提供 `PeerRequestOptions::with_timeout`：`rmcp-3.5.1/src/service.rs:786-790`。
7. `cancel_session_external_tasks` 的 20 秒 deadline 在逐任务 `cancel_async().await` 全部结束后才建立：`peri-middlewares/src/mcp/client/subscription_tasks.rs:34-46`。取消 RPC 本身没有期限，deadline 覆盖不到它。
8. `session/close` 通过 `pool.cancel_session_external_tasks(session_id).await` 执行取消：`peri-acp/src/host/requests/session_close.rs:112-118`；该调用处于上述全锁派发路径内。

**推断（有代码依据但未用真实无响应 MCP 进程复现）**

- 触发条件是 MCP 连接仍存活但 `tasks/cancel` 不回包（进程活着、请求未返回）。此时取消 future 永不完成，接收循环与全局 `sessions` 锁被长期占用。
- 本地 stdio MCP 返回 task 后同样走该 RPC，因此也可达；是否触发取决于 server 对 `tasks/cancel` 是否回包。
- 未实现该方法的 server 通常立即返回错误，不会进入无期限等待；本问题不依赖此分支。

## 根因链

1. 取消请求在 `ServerLoop` 接收循环同一 task 内执行（观察，证据 1）。
2. 该执行位于全局 `sessions` mutex 内（观察，证据 2）。
3. handler 等待外部 cancel 闭包（观察，证据 3、4）。
4. MCP 闭包等待 `tasks/cancel` RPC，但 rmcp 请求未设置 timeout（观察，证据 5、6）。
5. 取消等待没有期限，接收循环因此无法处理后续输入、取消、close/delete 等请求（推断，由 1–4 直接推出）。
6. `session/close` 的 20 秒预算只在逐个取消 await 之后开始，保护不到取消 RPC（观察，证据 7、8）。

用户可见后果：MCP owner 不回包时，`session/cancel-bg-task` 悬挂；同一连接上的控制面请求排队；`session/close`/`delete` 也可能长时间无响应，直到 MCP 进程退出或连接断开。

## 影响面

- 用户影响：取消后台任务无响应，TUI 输入/取消/关闭会话被阻塞；无 UI 级规避手段，只能结束 MCP 进程或断开连接。
- 受影响模块/调用点：
  - `peri-acp/src/host/server_loop.rs`（接收循环与 `sessions` 锁内派发）
  - `peri-acp/src/host/requests/session_tasks.rs`（取消 handler）
  - `peri-acp/src/host/requests/session_close.rs`（关闭路径）
  - `peri-middlewares/src/mcp/client/subscription_task_binding.rs`（MCP cancel 闭包）
  - `peri-middlewares/src/mcp/client/subscription_tasks.rs`（关闭预算）
  - `peri-agent/src/agent/async_tasks/manager.rs`（外部 cancel 闭包 await，辅助路径）
- 是否可规避：只能在外部结束 MCP 进程/断开连接，让 RPC 以 transport 错误结束；无协议面规避。

## 修复方案

- **范围**
  1. `peri-middlewares/src/mcp/client.rs` 新增 `MCP_TASK_CANCEL_TIMEOUT = 5s`。
  2. `subscription_task_binding.rs` 的取消闭包改用 `PeerRequestOptions::with_timeout` + `send_request_with_option` 发送 `tasks/cancel`，等待 `await_response`；超时由 rmcp 原生机制发送 `notifications/cancelled`，并返回可识别错误。非 `TaskAckResult`/`EmptyResult` 响应也返回错误；不伪造任务终态。
  3. `subscription_tasks.rs::cancel_session_external_tasks` 在循环前建立 20 秒总 deadline；每个任务的 `cancel_async` 只使用剩余预算，耗尽即记录 `cancellation budget exhausted`；后续终态等待复用同一 deadline，不再按任务串行累加。
  4. `server_loop.rs` 将 `session/cancel-bg-task` 从全锁 inline 分支移入 `HostTaskKind::SessionRequest` 的 spawn 路径：锁内只做 `sessionId`/`closing` 存在性校验并克隆会话环境配置，网络等待在锁外执行。响应仍按原请求 id 路由。
  5. `session_tasks.rs` 增加 ACP 层 10 秒外层期限（大于 MCP 5 秒，正常路径让 rmcp 原生超时先发取消通知）；即使其他外部 cancel 实现无期限，ACP 请求也有界返回 `background task cancellation timed out after 10s`。
  6. `session_lifecycle.rs` 的 `handle_cancel_bg_task` 重导出可见性从 `pub(super)` 调整为 `pub(crate)`，供 `ServerLoop` 调用。

- **明确不做**
  - 不修改数据库 schema、不新增表。
  - 不修改 ACP 协议字段或错误码语义（沿用现有参数校验与 `-32603` 取消失败）。
  - 不重构 `tasks/get` 轮询策略。
  - 不把所有 ACP 请求统一改为 spawn。
  - 不重构 `session/close`/`delete` 的锁结构。

- **回归测试**
  - `peri-acp/src/host/requests_workflow_cases_test.rs`
    - `cancel_bg_task_timeout_returns_error_and_keeps_task_unsettled`
    - `pending_cancel_does_not_block_control_requests`
  - `peri-middlewares/src/mcp/client/subscription_tasks_test.rs`
    - `cancel_session_external_tasks_uses_one_total_deadline`
  - `peri-middlewares/src/mcp/subscription_timeout_test.rs`
    - `cancel_task_rpc_returns_after_its_own_deadline`（真实 HTTP MCP fixture，收到 `tasks/cancel` 后不回包）

## 验收标准

1. 外部 cancel 闭包永久 pending 时，真实 `ServerLoop` 上的 `session/cancel-bg-task` 在有界时间内返回错误，不悬挂；任务状态仍为 `running` 且未结算。
2. 同一悬挂期间，`session/bg-tasks`、`session/input/snapshot`、`session/input/enqueue`、`session/cancel` 均能及时得到响应，证明接收循环与 `sessions` 锁已释放。
3. MCP owner 收到 `tasks/cancel` 但不回包时，取消 RPC 在 5 秒期限内返回错误。
4. 两个外部任务的关闭等待共享 20 秒总预算，不是每个任务 20 秒串行累加。
5. `cargo check`、相关 lib 测试、`git diff --check` 通过。

## 验证记录

以下为实现后回填的真实结果（分支 `fix/mcp-cancel-timeout`，基线 `e598d178`）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- subscription`
  → `34 passed; 0 failed`，含 `cancel_session_external_tasks_uses_one_total_deadline` 与 `cancel_task_rpc_returns_after_its_own_deadline`。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- cancel`
  → `40 passed; 0 failed`，含 `cancel_bg_task_timeout_returns_error_and_keeps_task_unsettled` 与 `pending_cancel_does_not_block_control_requests`。
- `workflow_cases`
  → `12 passed; 0 failed`。
- `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp -p peri-middlewares`
  → `Finished dev profile`，退出码 0。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check`
  → 初次检查有 4 处格式差异；执行 `fmt --all` 后复核通过。
- `git diff --check`
  → 无输出（通过）。
- `git status --short`
  → 仅 9 个范围内文件被修改（实现 6 个、测试 3 个），另新增本 issue 文件。

## 剩余锁范围与未覆盖风险

- `session/close`/`delete` 仍在 `server_loop.rs` 的全锁兜底分支内调用 `cancel_session_external_tasks`；取消 RPC 现在有 5 秒单任务期限、关闭结算有 20 秒总预算，但这两类请求在结算期间仍持有全局 `sessions` guard，接收循环会在该窗口内停止处理其他消息。此为既有结构，本轮未重构；已在修复范围中明确不做。
- ACP 外层 10 秒期限用于兜底非 MCP 外部 cancel 实现；若外层先于 MCP 原生 5 秒到期（正常不会），被丢弃的 future 不会发送 `notifications/cancelled`。
- 仅对 MCP `tasks/cancel` 增加了原生期限；其他外部取消实现由 ACP 10 秒外层期限保护，没有各自的传输级期限。
- `session/close` 对子会话调用 `close_child_resources`，每个子会话依次经过 `close()` → `close_agent_session_scope` → `cancel_session_external_tasks`，每层各有一份 20 秒预算且递归嵌套；多子会话时总等待会随子会话数量累乘，没有父级单一总期限。该路径同样在全局 `sessions` 锁内执行。
- 5 秒期限由 `PeerRequestOptions::with_timeout` 施加在 `await_response` 上，只覆盖响应等待；`send_request_with_option` 内发送入队的背压不在期限内，发送侧阻塞时取消等待仍可能超过 5 秒。
- `notifications/cancelled` 未在 wire 上断言，仅由 rmcp 源码确认超时路径会调用 `send_timeout_cancel_notification`（`service.rs:598`）；测试只覆盖错误返回与期限。
- 未做 TUI/真实 MCP 进程的端到端手测；wire 级验证使用 HTTP MCP fixture 与内存 ACP transport。
