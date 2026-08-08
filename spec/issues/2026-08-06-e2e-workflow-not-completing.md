# E2E: workflow 运行 5 分钟未出现完成状态

**状态**：Fixed
**优先级**：高
**类型**：缺陷
**创建日期**：2026-08-06
**来源**：E2E 全量运行（2026-08-06，`e2e/e2e-results-2026-08-06.md` 问题 5）

## 问题描述

两个 workflow 测试同时超时失败（均未重试）：

- `e2e/tests/workflow/workflow-panel-columns.test.ts`（322s）——「workflow 完成后 panel 中 agent 列值可见」
- `e2e/tests/workflow/workflow-run.test.ts`（321s）——「触发 workflow → /workflows 面板观察运行态 → 查看完成结果」

现象：均 `Text "completed. (" not found (timeout: 300000ms)`，workflow 运行 5 分钟未出现完成状态文本。两文件同时超时，疑似同一根因（workflow 执行链路或完成态渲染）。

## 现状

workflow 触发后应逐步显示运行态并最终出现完成文本 `completed. (`；当前 300s 内未出现。可能原因：workflow 执行链路（触发 → runner → 事件回传）、面板渲染完成态、或测试的 prompt 未触发 workflow 执行。

## 期望改进方向

- workflow 触发后正常执行，运行态在 `/workflows` 面板可见。
- 完成后出现 `completed. (` 完成文本，agent 列值可见。

## 验收标准

- [ ] `npm test -- tests/workflow/workflow-run.test.ts` 通过（从 `e2e/` 执行）。
- [ ] `npm test -- tests/workflow/workflow-panel-columns.test.ts` 通过（从 `e2e/` 执行）。

## 涉及文件

- `peri-workflow/src/runner.rs`、`peri-workflow/src/progress.rs` —— workflow 执行
- `peri-tui/src/kit/panels/workflow.rs`、`peri-tui/src/kit/workflow_snapshot.rs` —— 面板与状态
- `peri-tui/src/kit/acp_events/system.rs` —— workflow 事件
- `e2e/tests/workflow/workflow-run.test.ts`、`workflow-panel-columns.test.ts` —— 场景测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-06 | — | Open | agent | E2E 全量运行失败，创建 issue |
| 2026-08-08 | Open | Fixed | agent | 修复记录完整（downcast_arc 经 as_any 判定），两个 e2e 通过（55.5s/42.3s）；遗留项 McpPoolPort/ToolSearchPort downcast 同构 bug 于 2026-08-08 单独修复（ports.rs），状态未随修复同步更新，本次盘点闭环 |

## 修复记录

### 2026-08-06 修复（agent）

**根因**：`WorkflowMiddlewarePort::downcast_arc` 实现 bug（`peri-acp-types/src/ports.rs`，L5 批 2 引入）。trait 不继承 `Any`，对 `Arc<dyn WorkflowMiddlewarePort>` 直接调 `(*ptr).type_id()` 会命中 `Any` 的 blanket impl，返回 `TypeId::of::<dyn WorkflowMiddlewarePort>()`（trait object 自身），恒不等于 `TypeId::of::<T>()` → downcast **恒失败**。后果：`peri-middlewares::assembly` 装配中间件链时无法还原 session 级 `WorkflowMiddleware`，回退创建**临时实例**——WorkflowTool 注册的 registry 与 executor 完成通知消费者（`executor.rs` 单次 spawn，订阅 session 级 registry）分离。workflow 完成后 `registry.complete()` 广播无订阅者（日志：`WARN workflow: registry: notification send failed (no subscribers) error=channel closed`），Defer 完成通知永不入队，TUI 消息区永不出现 `completed. (`。

**排查依据**：
- 通过对比：同轮 `workflow-reporting.test.ts` 通过（46s，只轮询 `.claude/workflow-runs/*/state.json`，不依赖 TUI 文本），证明 workflow 执行链路正常（3 个失败 run 的 state.json 均 `status: completed`）。
- 运行日志（`.tmp/agent-tui.log`）显示 `registry: notification send failed (no subscribers)`，且 8 月 6 日 00:48–09:58 UTC 期间 consumer 正常（`AsyncRouter: routed workflow event to inbox`），13:23 UTC 起失效——回归点在 L5 系列提交。
- 临时加 WF-DIAG 日志复现：`downcast_ok=false`，consumer 已 spawn 且订阅（`consumer task started, subscribed`），证实 registry 分离。

**修复**：`downcast_arc` 改经 `as_any().type_id()` 取具体类型 TypeId（`WorkflowMiddleware::as_any` 返回 `self`），downcast 恢复生效，装配面复用 session 级实例，完成通知经 AsyncRouter → Defer → Receive（`SyntheticUserMessage`）→ TUI 用户气泡渲染。

**验证**：
- 新增回归测试 `workflow::tests::test_workflow_middleware_port_downcast_restores_concrete`（断言 downcast 还原同一 Arc）。
- 单元测试：peri-acp-types 86、peri-middlewares 1071、peri-agent 631、peri-acp 307、peri-workflow 43 全部通过；`cargo clippy -p peri-acp-types -p peri-middlewares -p peri-agent --all-targets -- -D warnings` 零警告。
- e2e（真实 LLM）：
  - `npm test -- tests/workflow/workflow-run.test.ts` → **通过，55.5s**（原 321s 超时）
  - `npm test -- tests/workflow/workflow-panel-columns.test.ts` → **通过，42.3s**（原 322s 超时）
- 修复后日志确认 `AsyncRouter: routed workflow event to inbox`，`no subscribers` 消失。

**修改文件**：
- `peri-acp-types/src/ports.rs` —— `WorkflowMiddlewarePort::downcast_arc` 用 `as_any().type_id()` 判定。
- `peri-middlewares/src/workflow/mod.rs` —— 新增 downcast 回归测试。

**遗留相关项**（未修，超出本 issue 范围）：`McpPoolPort` / `ToolSearchPort` / `CronSchedulerPort` 的 `downcast_arc` 存在同一 `type_id()` 写法（`ports.rs:47`、`ports.rs:67`、`cron.rs:63`），downcast 同样恒失败并回退 fallback（MCP 连接池不共享、tool search 索引不共享、cron 注入失效）。修复后行为会从 fallback 变为真实注入，需单独验证后再动。
