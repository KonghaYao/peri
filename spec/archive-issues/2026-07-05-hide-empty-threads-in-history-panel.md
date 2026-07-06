> 归档于 2026-07-06，原路径 spec/issues/2026-07-05-hide-empty-threads-in-history-panel.md

# ThreadBrowser 面板应隐藏 message_count 为 0 的空线程

**状态**：fixed
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

在 ThreadBrowser 面板（`/threads` / `/history`）中，`message_count == 0` 的空线程（刚创建但尚未发送任何消息）仍然出现在列表中，显示为 `(untitled xxx)` + `0 messages`。这些空线程没有可用信息，占据列表空间，造成视觉噪音，影响用户浏览和切换线程的体验。

期望：在 ThreadBrowser 面板中不显示 `message_count == 0` 的 thread。

## 症状详情

| 维度 | 当前行为 | 期望行为 |
|------|---------|---------|
| 空线程展示 | 显示 `(untitled abc12345)`，`0 messages`，日期为创建时间 | 不显示 |
| 列表可用性 | 空线程混在正常线程中，干扰查找 | 列表只包含有实际对话内容的线程 |
| 首次使用后 | 每创建一个新 thread 就会多一个空条目 | 新 thread 在发送第一条消息前不出现在列表中 |

## 涉及文件

- `peri-tui/src/kit/panels/thread_browser.rs` —— ThreadBrowser 面板组件，渲染 thread 列表
- `peri-tui/src/kit/service_snapshot.rs:158-179` —— 30 秒定时刷新 thread 列表，`ThreadMeta → ThreadSummary` 映射处
- `peri-tui/src/kit/atoms.rs:86-93` —— `ThreadSummary` 结构体（含 `message_count: usize` 字段）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（待修复后追加）
