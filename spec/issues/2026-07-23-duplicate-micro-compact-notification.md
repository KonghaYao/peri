# TUI 中出现连续两条重复的 Micro compaction 完成通知

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-23
**类型**：Bug

## 问题描述

在对话过程中触发 Micro Compact 后，TUI 消息流中会出现连续两条完全相同的 "Micro compaction completed（N 文件）" 提示。正常应只出现一条。

## 症状详情

```
● Shell (cd /Users/konghayao/code/ai/perihelion && git diff --cached --stat)
  ⎿ e2e/tests/panels/plugin.test.ts                    | 126 +++++++++++++++++++++
  ⎿ … 3 more lines
Micro compaction completed（5 文件）
Micro compaction completed（5 文件）
```

两条提示的内容、参数完全相同，紧挨出现。

## 复现条件

- **复现频率**：必现——每次 Micro Compact 触发均出现
- **触发步骤**：
  1. 在较长的对话中，上下文使用率超过 75% 阈值
  2. 自动触发 Micro Compact  
  3. 观察消息流中会出现连续两条相同的 compact 完成通知
- **环境**：任意模型、任意 OS

## 涉及文件

- `peri-acp/src/event/forwarder.rs:89-107` —— observe 事件双轨扇出，对 `MessagesCompacted` 同时走 v2_tx 和 on_event 两条路径
- `peri-tui/src/kit/v2_bridge.rs:108-131` —— v2 路径将 `ObserveEvent::MessagesCompacted` 映射为 `AcpEventData::CompactCompleted` 并注入 SystemNote
- `peri-agent/src/agent/events_v2_mapper.rs:169-187` —— v1 路径将 `ObserveEvent::MessagesCompacted` 映射为 `ExecutorEvent::CompactCompleted`，经 event_sink → acp_notifier 再次注入 SystemNote
- `peri-tui/src/kit/acp_events/compact.rs:14-79` —— `handle_compact_completed` 调用 `inject_system_note`，被两条路径各自调用一次

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-23 | — | Open | agent | 创建——排查 Micro Compact 重复通知时发现 |
| 2026-07-23 | Open | Fixed | agent | 修复：移除 v2_bridge 中 CompactStarted/MessagesCompacted 的双轨映射 |

## 修复记录

### 修复 #1（2026-07-23）

- **操作人**：agent
- **用户原意**：消除 TUI 中连续两条重复的 Micro compaction 完成通知
- **修复内容**：从 `v2_bridge.rs` 的 `v2_event_to_acp_event_data` 中移除 `CompactStarted` 和 `MessagesCompacted` 的映射分支，归入 None 臂。这两个事件已由 forwarder.rs 旧路径（`observe_event_to_executor` → `on_event` → `event_sink` → `acp_notifier`）完整覆盖，双轨扇出导致 `handle_compact_completed` 被调用两次、注入两条 SystemNote。
- **验证状态**：待验证
