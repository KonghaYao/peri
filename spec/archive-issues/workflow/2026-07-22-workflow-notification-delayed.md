# [Bug] Workflow 完成通知严重延迟（数据堆积）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-22

## 问题描述

在 Workflow 工具批量执行过程中，多个 workflow 实际完成后，完成通知不是实时抵达 Agent，而是在下一次用户消息时才集中到达。表现为 15-70 分钟的通知延迟。

## 症状详情

在本次 8-task P0+P1 修复过程中，观察到 Workflow 工具的**完成通知存在严重延迟**：

| Workflow | 实际完成时间 | 通知抵达时间 | 延迟 |
|----------|:----------:|:----------:|:----:|
| `fix-P1-5` 第一次 | 11:09 | 12:15+ | 60+ 分钟 |
| `fix-P0-3` | 11:18 | 12:15+ | 50+ 分钟 |
| `fix-P1-1` 第一次（被 kill） | 11:03 | 12:15+ | 70+ 分钟 |
| `fix-P0-3-tests` | 11:31 | 12:15+ | 40+ 分钟 |
| `fix-P1-2` | 11:47 | 12:15+ | 25+ 分钟 |
| `fix-P1-4` | 11:56 | 12:15+ | 15+ 分钟 |

**特征**：所有延迟通知几乎在同一时间（12:15）集中抵达，表现为批量堆积后一次性释放。

## 影响

1. **Agent 阻塞等待**：主 Agent 等待通知期间无法继续编排下一个 workflow，必须主动轮询 `journal.jsonl`
2. **误导性**：通知到达时 Agent 可能已完成新的操作，陈旧通知引起混淆
3. **吞吐量下降**：8 个 task 若完全依赖通知驱动，实际耗时可能数倍于应有时间

## 推测根因

可能是 Workflow runner 进程和主 Agent 进程之间的通知通道（MessageQueue？）存在背压或定时刷新机制，导致通知在缓冲区堆积，非实时推送。

## 复现步骤

1. 启动一个长时间运行的 workflow（~10 分钟）
2. 在 workflow 运行期间不向 Agent 发送新消息
3. 观察：通知是在 workflow 实际完成后立即到达，还是延迟到达

## 相关文件

- Workflow runner：`peri-workflow` crate
- 通知通道：需排查 `MessageQueue` / `system-reminder` 机制

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |
| 2026-07-22 | Open | Fixed | agent | 修复：将 bg.complete_workflow() 从 tool.rs 通知 task 移至 executor.rs 消费者 task 的 Defer 入队之后 |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：修复 Workflow 完成通知 15-70 分钟延迟问题，通知应实时到达
- **修复内容**：
  - `peri-workflow/src/tool.rs:329-331`：移除通知 task 中 `bg.complete_workflow()` 调用（成功和异常路径），仅保留 `registry.complete()` broadcast。添加注释说明移至 executor
  - `peri-acp/src/session/executor.rs:780,859-864`：在 broadcast consumer task 中捕获 `bg_registry`，在 Defer 入队（Path B）之后调用 `bg_registry.complete()` 递减 active_count
- **根因**：时序竞态——tool.rs 通知 task 在 `registry.complete()` (broadcast) 和 `bg.complete_workflow()` (active_count--) 之间无 `.await` 点。broadcast consumer 是独立 tokio task，可能尚未调度时 active_count 已归零，agent 的 `idle_should_wait` probe 返回 false → ReAct loop 退出 → Defer 堆积在 MessageQueue 中 → 下次用户消息 `drain_for_end()` 一次性释放
- **验证状态**：已通过（cargo build + clippy + test，pre-commit 全绿）
