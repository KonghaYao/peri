> 归档于 2026-08-11，原路径 spec/issues/2026-07-22-p1-1-acp-events-giant-dispatch-split.md

# P1-1：acp_events.rs 巨型 dispatch 函数拆分

**状态**：Fixed
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 模块化与分层维度

## 最新情况（2026-08-11）

对应实现已将 acp_events.rs 巨型 dispatch 拆分为 acp_events/ 9 子模块

## Problem Statement

`peri-tui/src/kit/acp_events.rs:235-1169` 中 `dispatch_and_notify` 函数为单一 934 行 match，根据 `AcpEventData` 枚举分发到不同 handler。随着事件类型增长（当前 ~15 个变体），该函数已成为全项目最大的单函数。

问题：
- 新增事件变体需修改该巨型 match，冲突概率高
- 难以定位特定事件的 handler 逻辑
- 无法对单个事件 handler 做单元测试
- 代码审查时难以判断改动范围

## 建议方案

将每个 match arm 提取为独立函数，按事件类型分组到子模块：

```
acp_events/
  mod.rs            — dispatch_and_notify 入口 + match 骨架
  turn.rs           — TurnStarted/TurnDone/TurnError handlers
  agent.rs          — AgentStarted/Stopped/ExecutionFailed handlers  
  tool.rs           — ToolStarted/ToolProgress/ToolEnded handlers
  compact.rs        — CompactCompleted handler
  system.rs         — SystemNotification/CacheWarning handlers
  subagent.rs       — SubagentStarted/Stopped handlers
  render.rs         — push_view_models 等渲染辅助
```

每个 handler 函数签名统一为 `fn handle_xxx(ctx: &mut DispatchContext, data: &AcpEventData)`。

## 涉及文件

- `peri-tui/src/kit/acp_events.rs` — 巨型函数所在文件

## 风险

- **低**：纯函数提取，不改变逻辑。注意 handler 间共享的辅助函数（如 `inject_system_note`）保持可访问
