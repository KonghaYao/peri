# Langfuse batcher 命令通道容量 = max_events（默认 50）+ DropNew：慢 flush 期间事件被静默丢弃

**状态**：Open
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S5.2

## 问题描述

`BatcherConfig { max_events, ... }`（`langfuse/session.rs:38-44`）同时决定 buffer 上限与命令通道容量：`mpsc::channel(config.max_events)`（`langfuse-client/src/batcher.rs:40`）。`try_add` 走容量仅 50 的命令通道（`batcher.rs:180-199` DropNew 丢事件）；`run_loop` 在 `do_flush`（HTTP + 3 次重试，最长 ~150s）期间**阻塞在 select 内**，无法消费 Add 命令——一轮 turn 事件 >50 且赶上 flush 时，后续事件（含父 span 之后的子 span）被静默丢弃，Langfuse 观测图出现悬挂/孤儿 span。

## 症状详情

- 触发场景：大工具输出轮次（tool batch 常见 20-60 事件）恰逢上一次 flush 慢（网络抖动/重试）
- 只 warn 不降级，无丢弃统计
- 与 `2026-07-25-control-boundary-events-can-be-dropped.md` 不同层：该 issue 是 EventBus 有界通道 try_send 丢事件（agent 内），本 issue 是 langfuse-client batcher 命令通道（遥测上报）

## 复现条件

- **复现频率**：偶发（慢 flush + 高事件量叠加）
- **触发步骤**：大工具输出轮次 + 网络抖动触发 HTTP 重试

## 涉及文件

- `peri-acp/src/langfuse/session.rs:38-44` —— `BatcherConfig` 容量=50
- `langfuse-client/src/batcher.rs:40` —— 命令通道容量
- `langfuse-client/src/batcher.rs:180-199` —— `try_add` DropNew
- `langfuse-client/src/batcher.rs:68-118` —— `do_flush` 阻塞 select

## 修复方向（对抗 review 优先级校准）

"通道容量与 buffer 上限分离"**只是延后丢弃**（慢 flush 时通道再大也会满——一次 ReAct 迭代的 TextChunk 数远超 50），根因是 `do_flush` await 期间 run_loop 无法消费 Add：

1. **优先（低成本可观测）**：DropNew 时记录"已丢弃 N 条"汇总事件/计数 + 汇总日志，保证父 span 不先于子 span 丢的可观测性
2. 可选优化：通道容量与 buffer 上限分离（如 `max_events * 4`），或改造单 select 结构让 flush 不阻塞消费

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-acp 审查发现；对抗 review 校准优先级） |
| 2026-08-05 | Open | Fixed | agent | 修复：DropNew 丢弃计数 + flush 后汇总日志 + dropped_count() 观测；通道容量分离评估后跳过（延后丢弃不解决根因，且放大 Shutdown 积压丢失窗口） |

## 修复记录

### 修复 #1（2026-08-05）

- **修复内容**（`langfuse-client/src/batcher.rs`）：
  1. `Batcher` 新增 `dropped: Arc<AtomicUsize>` 丢弃计数；`add`/`try_add` 的通道满（Full）与通道关闭（Closed）分支 `fetch_add(1)` 累加；
  2. `run_loop` 每次 `do_flush` 完成后调用 `report_dropped`：`swap(0)` 取出计数，>0 时输出 `tracing::warn!` "已丢弃 N 条（上一 flush 周期内命令通道满/关闭）" 汇总日志并清零——慢 flush 期间的静默丢弃变为可观测；
  3. 新增 `pub fn dropped_count()` 供调用方/测试观测。
- **通道容量分离（`max_events * 4`）评估：跳过**——代码上是 1 行改动，但：a) 只是延后丢弃（一次 ReAct 迭代的 TextChunk 数远超 50，慢 flush 时通道再大也会满）；b) `Shutdown` 分支不 drain 通道积压命令，容量放大 4 倍会放大关闭时的事件丢失窗口；c) 丢弃汇总已满足"低成本可观测"优先目标。
- **测试**（`batcher_test.rs`）：新增 `test_batcher_drop_new_increments_dropped_counter_during_slow_flush`——本地慢速 HTTP server（500ms 延迟）制造 run_loop 阻塞在 do_flush 的窗口，断言：通道满时丢弃 2 条且 `dropped_count()==2`，flush 完成后计数清零（汇总已输出），恰好 2 次 flush 请求。（mockito 1.x 无 `with_delay`，故用自定义慢 server）
- **涉及文件**：`langfuse-client/src/batcher.rs`、`langfuse-client/src/batcher_test.rs`
- **验证状态**：待验证（`cargo test -p langfuse-client --lib` 63 passed；`cargo clippy -p langfuse-client --lib -- -D warnings` 通过）
