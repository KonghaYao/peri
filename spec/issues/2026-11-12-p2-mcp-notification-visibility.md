# P2：MCP 自定义通知与日志不进入 Peri 会话

**状态**：已修复，待 PR/CI 验收。
**优先级**：P2。
**类型**：能力缺口（MCP 通知可观测性）。
**创建日期**：2026-11-12。
**来源**：`examples/mcp-use-showcase` 验收 + 独立 SDK 客户端对照。
**确认方式**：Peri 会话实测 + 官方 SDK 客户端复测 + 源码路径复核（基线 `e73856578`；工作区含未提交的 examples 变更）。

## 确认证据

- Peri 内调用 `slow_task {steps:3, stepMs:50}`，返回
  `任务 run-1 完成：3 步，进度通知支持=true。`
  —— 说明请求携带了 `progressToken`，server 已上报进度。
- 同一 server 用官方 `@modelcontextprotocol/client` 客户端调用
  `slow_task {steps:2, stepMs:10}`，`fallbackNotificationHandler` 收到：
  - `demo/task-status`：`started` → `running`（每步一条）→ `completed`；
  - `notifications/message`：`logger="showcase"`，
    `data={runId, steps, stepMs}`。
- Peri 会话在 `set_theme` / `reset_demo_state` 后能收到
  `subscription_resource_updated(uri=demo://state)`，说明订阅提醒通道正常；
  但整轮验收没有出现任何 `demo/task-status` 或 `notifications/message` 会话事件。
- 源码：`peri-middlewares/src/mcp/client/subscription.rs:247-249` 的兜底分支把
  `Cancelled / Progress / Logging / Task / Custom` 统一忽略；
  `grep ServerNotification::` 在 `peri-middlewares/src/mcp` 未发现其他
  Custom / Logging 会话出口。
- 协议注记：`docs/reference/mcp-ecosystem.md:396` 标明
  `notifications/message` 在 MCP 2026-07-28 已废弃（SEP-2577），仅当请求
  `_meta` 带 `io.modelcontextprotocol/logLevel` 才应发送。日志不展示不应单独
  判为缺陷；**自定义通知**是本文的主要缺口。

## 复现步骤

1. 启动 server：`cd examples/mcp-use-showcase && bun run dev`。
2. 在 Peri 中调用 `slow_task`，参数 `{"steps":3,"stepMs":50}`。
3. 观察工具返回与会话事件。
4. 对照：用官方 SDK 客户端（`scripts/client.ts` 或临时 `bun -e`）调同一工具，
   注册 `fallbackNotificationHandler` 并打印通知。

预期：Peri 会话至少能看到自定义通知的某种投影（TUI MCP/通知面板、调试日志或
低频系统提醒，按产品决策）。
实际：只有工具最终文本；`demo/task-status` 与 `notifications/message` 被丢弃，
没有可见事件；当天的 `~/.peri/logs/agent-tui.*` 中也未检索到相关记录。

## 根因链

1. `subscriptions/listen` 消费循环只把 `ResourceUpdatedNotification` 转为会话提醒
   （观察：`subscription.rs:208-231`）。
2. `ResourceListChanged` / `ToolListChanged` / `PromptListChanged` 只做缓存失效，
   不产生会话事件（观察：`subscription.rs:232-246`）。
3. 兜底分支 `Ok(Some(_))` 明确忽略 Progress / Logging / Task / Custom
   （观察：`subscription.rs:247-249`）。
4. 工具调用响应流上的自定义通知也没有独立会话出口；MCP 客户端通知入口目前只
   覆盖订阅与资源转发（观察：`grep ServerNotification::`）。
5. 因此无论通知从 listen 流还是请求流到达，Peri 都没有用户/模型可见的展示面；
   现有日志级别下也没有可检索记录，表现为静默丢弃。

## P2 依据

- 不影响工具执行与数据一致性；影响服务端状态可观测性与验收准确性。
- 有规避手段：看工具最终结果、用独立客户端复测、查 server 日志。
- 若产品明确不需要在宿主展示自定义通知，可降级为文档缺陷并关闭代码项；
  但当前展示策略未成文，验收指南因此产生过误判。

## 影响面

- 用户影响：长任务 server 无法通过 MCP 自定义通知暴露状态；排障只能看最终结果。
- 验收影响：showcase 指南原先要求“收到 `demo/task-status` /
  `notifications/message`”，会把宿主展示缺口误判成 server FAIL。相关 skill 已
  在本次验收中同步修正。
- 受影响模块：`peri-middlewares/src/mcp/client/subscription.rs`、MCP 通知事件
  模型、TUI MCP/通知面板。
- 不受影响：`resources/updated` 订阅提醒、工具最终结果、MRTR 能力协商。

## 修复方案（建议）

1. 明确产品策略：自定义通知不唤醒模型，但应在 TUI 的 MCP/通知面板或日志中可见；
   如需模型感知，使用低频 system reminder，避免每条通知触发一次推理。
2. 在通知消费路径新增 `CustomNotification` 分支：按
   `server + method + params` 生成结构化事件，交给 TUI/事件 sink；至少补
   `tracing::debug!`，禁止静默丢弃。
3. `notifications/message` 按 2026-07-28 处理：只有请求 `_meta` 声明 logLevel
   时才展示/转发；未声明时忽略并记录 debug，不作为缺陷。
4. `ProgressNotification` 可复用现有工具进度展示，不要求进入模型上下文。
5. 回归测试：
   - fake MCP server 在 `subscriptions/listen` 流上发自定义通知，断言事件到达
     TUI/通知 sink；
   - 在 `tools/call` 响应流上发自定义通知，断言同样可达；
   - 未订阅的类别不得外溢；
   - `resources/updated` 提醒保持不变。

## 验收标准

- server 发送的 custom notification 在选定展示面可见，包含 server 与 method；
- 不引入每条通知唤醒模型的行为；
- `resources/updated` 回归通过；
- showcase 验收能按新 skill 得到 PASS / SKIP，而不是 FAIL。

## 非目标

- 不在本文实现 MRTR sampling / elicitation（宿主当前 `sampling=false`、
  `elicitation=false`，属独立能力缺口）。
- 不修改 `examples/mcp-use-showcase` 的 server 行为。
- 不为 `notifications/message` 恢复已废弃的 2025-11-25 语义。

## 交接

- 修复方：隔壁 MCP / 通知链路任务。
- 相关 skill 已更新：
  `examples/mcp-use-showcase/skills/showcase-test-guide/SKILL.md`、
  `examples/mcp-use-showcase/src/testing-guide.ts`、
  `examples/mcp-use-showcase/skills/showcase-test-guide/references/failure-triage.md`。
- 修复后请回填“验证记录”，并同步 `docs/reference/mcp-ecosystem.md`
  （若展示策略属于权威契约）。

## 验证记录

### 实际改动

1. `peri-middlewares/src/mcp/client/service.rs`：新增 `PeriClientHandler { info, tap }`，替换
   `McpServiceWrapper::Default` 的 handler 类型；自定义 / 日志通知转 `NotificationTap`，
   进度 / 任务状态 / 取消只记 debug（不入缓冲）。
2. `peri-middlewares/src/mcp/client/notification_tap.rs`（新）：`NotificationTap` +
   `PendingMcpNotification` + 缓冲 drain + canonical reminder 映射。按 `(server, method)`
   合并、payload 截断到 2 KiB、缓冲上限 32；ACP 归属 server 只记 debug。
3. `peri-middlewares/src/mcp/client/transport.rs`：新增 `serve_client_auto_for_server`
   （initialize / reconnect 使用）；OAuth 重连路径（`client_oauth.rs:229-241`）直接调用
   `rmcp::service::serve_client` 并传入 `PeriClientHandler` + `NotificationTap`；
   `serve_client_auto` 保留为无 tap 入口（ACP 声明连接 / 测试直连）。
4. `peri-middlewares/src/mcp/client.rs`：新增 `pending_notifications` 与
   `declared_log_levels`；`peri-middlewares/src/mcp/middleware.rs` 在 `before_model`
   同一 `push_status_changes` 点位 drain，产出 `Diagnostic` / `custom_notification` /
   `Info` / `Configurable` / `[Tui, Diagnostics]` 提醒。
5. `notifications/message` 门控：`tool_bridge` 只在请求 `_meta` 已含
   `io.modelcontextprotocol/logLevel` 时记录；未声明时 debug 后忽略。Peri 不主动声明。
6. `subscription.rs` / `instance_face.rs` 兜底分支补 debug；`resources/updated` 行为未改。

### 测试命令与结果

```text
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::notification_tap
# 8 passed（T1 / T2a / T2b / T4 / ACP 隔离 / 合并 / 上限 / 截断）

./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::middleware::tests
# 45 passed（含 before_model 自定义通知 Info 提醒；无连接状态变化时仍 drain）

./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- builtin_subscription_wire
# 5 passed（T3：T7b 增加 ExternalEvent / Required / 受众 / summary / metadata / body 逐字段断言）

./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::subscription
# 35 passed（非 git 正常路径补 ExternalEvent / Required / 受众 / summary / metadata / body 全字段断言）
```

另外通过：`mcp::client::instance_face_tests` 8、`mcp::client::output_store` 20、
`mcp::tool_bridge` 26、`mcp::client::service::tests` 2、`mcp::client::transport::tests` 4、
`mcp::client::readiness::tests` 17、`mcp::initialize::` 31、`mcp::acp` 13；
`bash scripts/check-changed.sh` 与 `git diff --check` 通过。

### 未验证项

1. 未做真实 TUI 渲染验收（按要求不启动 Peri TUI）；提醒字段按 canonical DTO 断言。
2. 未用真实 `examples/mcp-use-showcase` 复测（禁止修改/触碰 examples）。
3. `notifications/message` 的 debug 日志未做日志捕获断言（仓库无 tracing 测试依赖）；
   测试只断言未声明时不入缓冲、已声明时入缓冲。
4. 未覆盖 wasm / emscripten 目标编译。
5. listen 流上的自定义通知按 rmcp 3.9.0 语义不承载；未改补丁，回归只覆盖
   `resources/updated` 标准通知。

### 已知限制

1. `declared_log_levels` 按 server 粘性保存（`client.rs:171`）：声明值不随请求生命周期
   结束清除，重连 / 移除 server 也不清理（`notification_tap.rs:100-104` 只写不删）。
   当前生产没有任何请求写入 `io.modelcontextprotocol/logLevel`（Peri 不主动声明），
   因此 `notifications/message` 门控实际恒走忽略分支；现状**不满足** per-request 隔离。
   若未来开启日志通路，需要改为按请求生命周期门控，并在重连时清理该 server 的声明。
2. `pending_notifications` 是部署级全局缓冲，任一会话的 `before_model` 先 drain 即进入该
   会话；多会话并发时，通知来源的会话不一定是展示会话。
3. 缓冲满 32 条时丢弃新通知并记 `tracing::warn!`，没有丢弃计数或背压。
4. 截断前先对 `params` 完整 `to_string()` 序列化，超大通知会在该连接的 service loop 上
   产生瞬时大分配。

### 裁决记录

1. 不改 rmcp 补丁；listen 流不承载自定义通知，可见性统一经 `ClientHandler` 汇合点。
2. ACP 声明 server 无会话归属，通知只写 `tracing::debug!`，不进入部署级缓冲。
3. 不主动在请求 `_meta` 声明 logLevel；未声明时 `notifications/message` 忽略并记 debug。
4. 不新增 TUI 通知面板；使用 canonical reminder（`Diagnostic` / `custom_notification` /
   `Info` / `[Tui, Diagnostics]`，不含 `Model`），`delivery` 不为 `DiagnosticOnly`。
5. 所有被丢弃/无法路由的自定义通知至少保留 `tracing::debug!`。

