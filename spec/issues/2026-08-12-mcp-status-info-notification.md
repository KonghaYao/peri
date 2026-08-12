# MCP 状态信息注入：首 turn 概览 + 运行中上下线通知

**状态**：✅ 已修复（2026-08-12）
**优先级**：中
**类型**：feature
**创建日期**：2026-08-12
**来源**：用户需求（"会话第一次发送信息的时候更新一下状态，以 system reminder 形式将 MCP 基础情况加入 user prompt，但不可过度抢占风头"）
**最后核查**：2026-08-12

## 需求

MCP 中间件在会话首次发送信息时，把 MCP 基础情况（服务器 + 状态 + 工具数）以 system reminder 形式注入模型上下文；之后每次 MCP 服务器上下线，各推送一条状态变化消息。约束：

- 失败统一报"名字 + 错误"即可。
- 注入频次是事件驱动的：连接变化即推送，不轮询。
- 摘要不包含资源信息；提示 agent 经 tool search 机制使用 MCP 工具（`mcp__<server>__<tool>` 格式）。
- 模型侧注入用 **Info 级别**消息（`MessageKind::Info`），不是 defer——不唤醒循环、不触发特殊操作。
- 首 turn 概念保留：初始化完成后的初始连接结果由首 turn 概览一次性覆盖，不逐条推送。

## 设计

### 注入时机

- **首 turn 概览**：`executor_helpers.rs` Phase 6.2（用户 Prompt push 之后）入队 Info 消息——`before_agent` 在首次 Receive 之后才执行、其产物要下个 turn 才可见，而 Phase 6 入队保证**同一 turn 首轮模型调用即可见**。**顺序语义**：Info 在 Prompt **之后**入队，Receive drain（FIFO）后模型看到 user 输入在前、`<system-reminder>` 紧随其后——"加入到 user prompt"语义，不抢在用户输入前（初版在 Prompt 前入队，导致 reminder 排在用户输入前面，2026-08-12 修复）。判定：`is_first_turn = !req.continuation && req.history.is_empty()`。
- **运行中状态变化**：`McpMiddleware::before_model` 每轮迭代 drain 缓冲队列，逐条 push Info。变化发生在空闲期 → 下个 turn 首轮 Receive 消费即见；运行中 → 下轮迭代即见。延迟一轮可接受（"不抢风头"）。

### 状态机（`McpClientPool`）

- 新增 `initialized: AtomicBool`：`run_initialize` 收口处 `mark_initialized()`。**初始化完成前**的状态写入不产生通知（初始连接结果由首 turn 概览覆盖，避免重复推送）；初始化后迟到的连接成功自然成为运行中变化。
- 新增 `pending_changes: Mutex<Vec<String>>` 缓冲 + `notifier: RwLock<Option<Fn(&str)>>` 回调。
- `record_status_change(name, old)` 统一出口：未初始化 / 无旧状态 / 新旧同值 → 静默；否则生成文本（Connected 带工具数、Failed 报名字+错误）入缓冲并回调 notifier。
- 赋值点改造：`insert_failed` / `insert_needs_auth` / `reconnect` / `client_oauth`（授权后 Connected、clear 后 Failed）插入前捕获 old、插入后 record。

### 事件面（TUI 通知显示）

- `ExecutorEvent::SystemNotification { text, level }` 新变体 → `event_sink.rs` Category ③ → `AcpEvent::SystemNotification`（wire `system_notification`）→ TUI `convert_agent_event` → `AcpEventData::SystemNotification` → `inject_system_note`（level: "info" → Info 样式）。TUI 显示端（system.rs）已有处理，仅补 notifier 映射分支。
- 装配：`assembly.rs` ChainSlot::Mcp 注入 notifier 闭包（`ctx.bg_event_tx`），middleware 不直接依赖事件系统。

## 实现要点

- `Middleware` trait 新增 `first_turn_reminder` 可选钩子（默认 `Ok(None)`）；`MiddlewareChain::run_first_turn_reminders` 顺序收集非空贡献，任一 Err 短路。
- `McpMiddleware`：`overview_text()`（按状态分组、失败带错误、无服务器返回 None）+ `push_status_changes()`（Info/SystemInjected 入 v2 queue，首条附 tool search 提示，`hint_sent` 每实例一次）。
- 事件面新增变体同步：`langfuse/bridge.rs` 忽略组、`event/mapper.rs` no-op 组补分支（编译期 exhaustive match 强制）。

## 验证

- `cargo test -p peri-middlewares --lib -- mcp::middleware`：10 passed（overview 格式、record_status_change 边界、drain 恰好一次、before_model Info push、tool search 提示每实例一次）。
- `cargo test -p peri-agent --lib -- middleware::chain`：23 passed（含 first_turn_reminder 收集顺序 / None 跳过 / 默认实现 / Err 短路）。
- `cargo test -p peri-tui --lib -- kit::acp_notifier`：24 passed（含 SystemNotification 透传测试）。
- 全量回归：6 个相关 crate `--lib` 全部通过（327/96/665/113/1228/1107）；`cargo clippy --all-targets -- -D warnings` 通过；`cargo fmt --check` 通过。

## 涉及文件

- `peri-agent/src/middleware/trait.rs`、`chain.rs`、`chain_test.rs` —— 新钩子 + runner + 测试。
- `peri-agent/src/session/exec/executor_helpers.rs` —— 首 turn 判定 + Phase 6 Info 入队。
- `peri-middlewares/src/mcp/client.rs`、`initialize.rs`、`reconnect.rs`、`client_oauth.rs` —— 状态机字段/方法/赋值点。
- `peri-middlewares/src/mcp/middleware.rs`、`middleware_test.rs` —— 中间件实现 + 测试。
- `peri-middlewares/src/assembly.rs` —— notifier 注入。
- `peri-acp-types/src/event.rs`、`peri-acp/src/event/mod.rs`、`event_sink.rs`、`event/mapper.rs`、`peri-controller/src/langfuse/bridge.rs` —— SystemNotification 事件面。
- `peri-tui/src/kit/acp_notifier.rs`、`acp_notifier_test.rs` —— convert_agent_event 映射 + 测试。
- 顺带：`peri-tui/src/kit/acp_events/streaming.rs` —— clippy redundant_closure 清理（任务 1 遗留）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-12 | — | 已修复 | agent | 设计拍板 + 实现 + 测试，随提交落地 |
| 2026-08-12 | 已修复 | 已修复 | agent | 顺序修正：reminder 在用户输入**之后**入队（初版在 Prompt 前入队导致 system reminder 排在用户输入前面） |

## 修复记录

| 日期 | commit | 说明 |
|------|--------|------|
| 2026-08-12 | 随本次提交 | 首 turn 概览 + 上下线 Info 推送 + SystemNotification 事件链路 |
