# Cron 工具创建的任务到点完全不触发（无任何信号）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-07

## 问题描述

TUI 主进程中，agent 会话通过 cron 工具（`cron_register`）创建的定时任务，到点后完全不触发：没有用户消息注入、没有报错、没有任何日志信号。必现，且无论 agent 处于空闲还是运行中都一样。今天（2026-08-07）刚出现，之前 cron 功能正常。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| 到点完全不触发 | 任务注册成功（`cron_register` 返回任务 id），但到点后无任何反应，用户消息不注入 |
| 完全无信号 | 无报错弹窗、无 warn/error 日志——对比 2026-08-04 旧 issue（`cron-trigger-lost-after-turn-error`）中 tick 循环会打 `warn!("cron tick: extra trigger sender closed, removing")`，本次连该 warn 都未出现 |
| 空闲/运行中都不触发 | 不是旧 issue 的"turn 结束后丢失"场景（旧场景运行中能触发、空闲时丢失）；本次是任何状态下都无触发 |
| 今天刚出现 | 2026-08-07 开始，此前正常；最近提交中无 cron 相关标题变更，`1fcacf75 Feature/3.0 (#81)` 大合并为时间线嫌疑（待排查确认） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. TUI 主进程启动（`CronState::spawn_tick_task` 每秒 tick）
  2. agent 会话中让模型调用 `cron_register` 工具注册定时任务（如 `*/1 * * * *` 每分钟触发）
  3. 等到点 → 任务不触发，无任何信号
- **环境**：TUI 主进程（peri-tui 内嵌 peri-acp 同进程共享 `Arc<Mutex<CronScheduler>>`）

## 涉及文件

- `peri-middlewares/src/cron/tools.rs` —— `cron_register` 工具：模型创建任务的入口，调用 `scheduler.register(expression, prompt)`
- `peri-middlewares/src/cron/mod.rs` —— `CronScheduler` 核心：`register()` / `subscribe()` / `tick()` / sender 清理
- `peri-tui/src/app/cron_state.rs` —— TUI 侧：`CronScheduler::new(unbounded_channel().0)`（primary trigger tx 被丢弃）+ `spawn_tick_task` 每秒 tick
- `peri-acp/src/session/cron_bridge.rs` —— session 级 cron bridge（2026-08-04 修复合入 commit `a22a3820`，旧问题修复产物，需确认其在新场景下的行为）

## 待排查方向（修复阶段确认）

- 任务是否真正进入 scheduler 的任务列表（`register` 成功但 tick 是否遍历到）
- tick 循环是否健康（是否还在跑、`next_tick` 计算是否正确）
- 触发后消息投递链路是否断开（trigger sender → bridge → prompt_tx → agent 队列），以及为何无 warn 日志
- 是否与最近代码变更相关（`1fcacf75 Feature/3.0 (#81)` 大合并 或之后提交）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-07 | — | Open | agent | 创建（用户报告 cron 工具创建的任务到点完全不触发；必现、无信号、今天刚出现） |
| 2026-08-07 | Open | Fixed | agent | 修复：`CronSchedulerPort::downcast_arc` 改经 `as_any().type_id()` 判定（`peri-acp-types/src/cron.rs:63`）；回归测试 `test_cron_scheduler_port_downcast_restores_concrete`；workspace build + cron/session/cron_owner 测试全绿，clippy 干净 |

## 修复记录

### 修复 #1（2026-08-07）

- **操作人**：agent
- **用户原意**：cron 工具（`cron_register`）创建的任务到点应正常触发并注入 agent
- **修复内容**：
  - **根因**：`CronSchedulerPort::downcast_arc`（`peri-acp-types/src/cron.rs:56-70`）直接对 trait object 调 `(*ptr).type_id()`——trait 不继承 `Any`，方法经 `Any` blanket impl 解析返回 `TypeId::of::<dyn CronSchedulerPort>()`（trait object 自身），恒不等于具体类型 → **downcast 恒失败**。后果：`assembly.rs:124-134` 回退创建临时 `CronScheduler`（实例 C），`cron_register` 工具注册到 C；而 tick 跑在 host 装配的 scheduler B 上、bridge 订阅的也是 B——C 无 tick 驱动、触发无处投递，**完全静默**。此为 `2026-08-06-e2e-workflow-not-completing.md` 修复记录中明确点名的遗留项（"cron 注入失效"），同根因同构。
  - **改动**：`peri-acp-types/src/cron.rs` `downcast_arc` 改经 `(*ptr).as_any().type_id()` 取具体类型 TypeId（与 workflow 修复同构）；`peri-middlewares/src/cron/mod_test.rs` 新增回归测试 `test_cron_scheduler_port_downcast_restores_concrete`（断言还原原 Arc，`Arc::ptr_eq`）。
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：已验证（cargo build --workspace 通过；cron 19 测试 / peri-acp session 155 / peri-agent cron_owner 4 全绿；clippy -D warnings 干净）

**遗留说明**（未修，超出本 issue 范围）：
- 同根因未修项：`McpPoolPort` / `ToolSearchPort` 的 `downcast_arc` 仍是 `type_id()` 写法（`ports.rs:44`、`:64`），downcast 同样恒失败（MCP 连接池、tool search 索引 fallback）——与本次 cron 修复无直接关系，另立 issue 处理
- idle 期（turn 结束后）cron 触发仍无消费方（旧 issue `2026-08-04` 修复方向 2 out of scope）：修复后运行中触发恢复；空闲时触发消息滞留队列，待下次用户输入时消费（行为与 2026-08-04 issue 记录一致，未改变）
