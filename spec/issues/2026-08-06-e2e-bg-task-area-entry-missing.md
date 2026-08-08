# E2E: bg subagent 派发后 BgTaskArea 运行条目未出现

**状态**：Fixed
**优先级**：中
**类型**：缺陷
**创建日期**：2026-08-06
**来源**：E2E 全量运行（2026-08-06，`e2e/e2e-results-2026-08-06.md` 问题 3）

## 问题描述

`e2e/tests/subagent/bg-task-area.test.ts` 失败（75s，未重试）。测试覆盖 bg subagent / bg shell / bg fork 运行期间展示栏可见、完成后 ✔。

- 位置：`tests/subagent/bg-task-area.test.ts`（`waitFor(/◎ agent/)`，timeout 60s）
- 现象：`等待 bg subagent 运行条目超时 (timeout: 60000ms)`——BgTaskArea 未出现 `◎ agent` 运行条目

## 现状

bg subagent 派发后，BgTaskArea 应在 60s 内出现 `◎ agent` 运行条目；当前未出现。可能原因：派发未发生、运行条目事件未到达 TUI、`◎ agent` 文本格式与渲染不符、或派发时序（思考 + 派发约 10-15s）超窗。

## 期望改进方向

- bg subagent 派发后 BgTaskArea 出现 `◎ agent` 运行条目（格式与测试断言一致）。
- bg shell / bg fork 同理；完成后条目出现 ✔。

## 验收标准

- [ ] `npm test -- tests/subagent/bg-task-area.test.ts` 通过（从 `e2e/` 执行）。

## 涉及文件

- `peri-tui/src/kit/bg_task_area.rs` —— BgTaskArea 渲染
- `peri-tui/src/kit/app_shell.rs` —— 派发入口
- `peri-agent/src/session/subagent.rs`、`peri-agent/src/agent/async_tasks.rs` —— bg 任务派发与事件
- `e2e/tests/subagent/bg-task-area.test.ts` —— 场景测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-06 | — | Open | agent | E2E 全量运行失败，创建 issue |
| 2026-08-08 | Open | Fixed | agent | 修复记录完整（stage_builder 工具注入时序），e2e bg-task-area 通过 119.3s；状态未随修复同步更新，本次盘点闭环 |

## 修复记录

（由修复 agent 修复阶段追加，创建时留空）

### 2026-08-06 修复

**根因**：`SubAgentTool`（Agent 工具）在生产路径拿不到运行时 host，`run_in_background: true` 被静默降级为同步执行。

具体机制（L5 归位后回归）：`peri-agent/src/session/exec/stage_builder.rs` 的 `build_stage_context` 中，工具注入（`chain.collect_tools` → `SubAgentMiddleware::build_tool`）发生在主 session 创建与 `mw.set_parent_session(session)` **之前**；`SubAgentTool::build_tool` 构建时读取 `parent_session` 恒为 None，`SubAgentTool::host()` 回退到空的 tool 回退 host（`task_manager` 等运行时通道全空）。`invoke` 中 `run_in_background && host.task_manager.is_some()` 不成立 → 静默走同步路径 → 不注册 `BackgroundTaskRegistry` → 无 `BgRegistryEvent::Started` → 无 `bg-task-started` 事件 → BgTaskArea 无 `◎ agent` 条目。

日志佐证（`agent-tui.2026-08-06`）：`SubagentStarted ... is_background=false`（同步派发）、全程无 `bg-task-started`/registry 事件。

**修复**（最小改动，`peri-agent/src/session/exec/stage_builder.rs`）：
- 将 `chain.collect_tools` 工具注入块从「session 创建前」移到「`set_parent_session` 注入后、`builder.build()` 前」；`StageContext` builder 构造相应后移（`chain`/`shared_tools` 在 collect_tools 借用后被 move 进 builder）。
- 新增时序契约注释：工具注入必须晚于 parent_session 注入，顺序不可调换。

**回归测试**：`peri-middlewares/src/subagent/mod_test.rs` 新增 `test_build_tool_after_set_parent_session_reads_runtime_host`——验证 `set_parent_session` 后 `build_tool` 的 tool 能经 parent_session 读到 session 级 host（task_manager）。

**验证结果**：
- `cargo test -p peri-agent --lib`：631 passed
- `cargo test -p peri-middlewares --lib subagent`：131 passed（含新增回归测试）
- `cargo clippy -p peri-agent -p peri-middlewares --all-targets -- -D warnings`：通过
- e2e `npm test -- tests/subagent/bg-task-area.test.ts`：**通过**（119.3s；bg subagent / bg shell / bg fork 三阶段全部通过，judge 5/5 pass）

**修改文件**：
- `peri-agent/src/session/exec/stage_builder.rs`（时序修复）
- `peri-middlewares/src/subagent/mod_test.rs`（回归测试）

