# BackgroundTaskCompleted 事件在 EventSink 无映射：注释声称的"Path A 通知条"是死路径

**状态**：Open
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S5.1

## 问题描述

`ExecutorEvent::BackgroundTaskCompleted` 在 `event_sink.rs` 的 match 中落入 `_ => None`（`:269`），`map_event`（Category ①）也只有 7 个变体——**事件 100% 死路**。`executor.rs:1066-1068` 与 `1149-1181` 注释宣称"Path A (TUI): 通过 EventSink 直推 BackgroundTaskCompleted → 通知条"，实际两个生产 send 点（`execute_bg.rs:374`、`executor.rs:1178`）都白推。TUI 的 bg 完成通知实际由 registry 通道（`BgRegistryEvent::Completed` → `system.rs:265` → SystemNote）提供，死路径被冗余掩盖。

## 症状详情

- **补映射无效**：TUI 的 `handle_background_task_completed`（`agent.rs:19-41`）**只打日志什么都不做**——补映射后通知条依然不出现，且与 registry 通道**双通道重复**风险
- 同族死变体：`StateSnapshotMeta`、`WorkflowProgress`、`LlmRetrying`、`ContextWarning`、`BgToolStep`、`LspDiagnostics`（TUI 侧 `convert_agent_event` 均有消费分支，peri-acp 侧均无生产者）
- 违反 ARC-EVENT-001（发射 → 映射 → 消费全链覆盖）；`caps.agent_event` 门控的 `context_usage`/`workflow` 能力对非 TUI 客户端静默失效

## 复现条件

- **复现频率**：必现（静态死路径）
- **触发步骤**：任何依赖 `peri/agent_event` 通道的消费者（stdio IDE、第三方）收不到 bg 完成/上下文用量/工作流进度

## 涉及文件

- `peri-acp/src/session/event_sink.rs:183-270` —— `_ => None` 吞掉变体
- `peri-acp/src/session/executor.rs:1066-1068,1149-1181` —— Path A 直推（死代码）
- `peri-acp/src/session/event/mod.rs:106` —— 变体定义
- `peri-tui/src/kit/acp_events/agent.rs:19-41` —— 消费方只打日志

## 修复方向（对抗 review 重设计）

"补映射"是死路，正确选项：

- **删 `executor.rs:1148-1181` 的 Path A 直推** + 评估 `ExecutorEvent::BackgroundTaskCompleted` 变体整体废弃（agent 续跑走 `on_bg_complete` 回调、通知走 registry 通道）
- 同步清理误导注释与同族死变体（或标注"恒无生产者"）
- 注意 `variant_coverage_test.rs` 受变体处理变化影响

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-acp 审查发现；对抗 review 判定补映射为死路） |
| 2026-08-05 | Open | Fixed | agent | 修复：删除 executor.rs Path A 直推 BackgroundTaskCompleted（死路径）+ 清理误导注释；变体评估后暂保留（executor.rs:708 push_done 触发依赖 + TUI 侧 5 文件/3 测试文件引用，废弃面过大，记录为后续项） |

## 修复记录

### 修复 #1（2026-08-05）

- **修复内容**：删除 `executor.rs` workflow 通知消费者（`subscribe_notifications` spawn）中的 Path A 直推（`notify_sink.push_event(BackgroundTaskCompleted)`），同步清理误导注释（"双路径"→ 单路径，删除 "[NOTE] 自动 continuation 需 TUI 侧处理 BackgroundTaskCompleted"）。`notify_bg.complete(...)`（registry 通道 → `BgRegistryEvent::Completed` → `bg-task-completed` unstable event → TUI 通知条）保留——这是真实通知路径，未被破坏。`on_bg_complete` 回调（agent 续跑）不受影响。
- **变体废弃评估结论（暂不废弃，记录为后续项）**：
  1. `executor.rs:708` bg event pump 用 `matches!(bg_event, ExecutorEvent::BackgroundTaskCompleted(_))` 触发 `push_done`（BgCommand/Immediate 命令路径 bg 完成后解除 TUI loading）——**功能性依赖**，删除变体需替代触发机制（如改由 registry 泵触发），属行为变更，超出本切片范围；
  2. 引用面：`peri-tui` 5 个文件（`acp_types.rs`、`acp_notifier.rs`、`acp_bridge.rs`、`acp_events/mod.rs`、`acp_events/agent.rs`）+ 3 个测试文件（`tool_test.rs` 6 处、`mapper_test.rs`、`concurrent_bg_agent_test.rs`）——废弃需跨 TUI 侧联动修改；
  3. `variant_coverage_test.rs` 已读：只断言 7 个 Category ① 变体 + `_ =>` wildcard 存在，**不**断言"所有变体都有处理"，废弃变体不破坏该测试。
- **涉及文件**：`peri-acp/src/session/executor.rs`（1063-1068 注释、1148-1181 Path A 直推）
- **验证状态**：待验证（`cargo build -p peri-acp -p peri-middlewares` 通过；`cargo test -p peri-acp --lib` 415 passed；`cargo test -p peri-middlewares --lib` 1099 passed；`cargo clippy -p peri-acp -p peri-middlewares --lib -- -D warnings` 通过）
