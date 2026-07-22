# Todo 区域多条目时显示空白


> 归档于 2026-07-20，原路径 spec/issues/2026-07-07-todo-multi-item-blank-display.md
**状态**：Fixed
**分类**：Bug
**严重级别**：P3
**创建日期**：2026-07-07

## 问题描述

当 agent 使用 TodoWrite 工具创建或更新包含多个条目的 todo 列表时，TUI 底部的 Todo 区域显示异常：
- **预期**：显示最新的 Todo 列表（带条目内容与状态图标）
- **实际**：显示空白（无任何条目渲染）

## 症状详情

| 触发场景 | 表现 | 频率 |
|----------|------|------|
| 单条目 todo | 正常显示 | — |
| 多条目 todo（≥2） | Todo 区域空白 | 高概率 |

## 数据流（端到端）

1. **TodoWrite.invoke()** — 全量覆盖 todo，通过 mpsc channel 发送通知
2. **TodoMiddleware** — 注册工具，持有 `notify_tx`
3. **Builder** — 创建 `(todo_tx, todo_rx)` channel，分别给 Middleware 和 V2AgentOutput
4. **Todo Forwarder** (`executor_helpers.rs`) — 从 `todo_rx` 读取 → 转换为 `ExecutorEvent::TodoUpdate`
5. **Event Mapper** (`event/mapper.rs`) — `TodoUpdate` → `SessionUpdate::Plan(plan)`
6. **ACP Client** (`acp_client/client.rs`) — 接收 → 转发为 `AcpNotification::SessionUpdate`
7. **ACP Notifier** (`acp_notifier.rs`) — 匹配 `tag == "plan"` → `handle_plan_update()`
8. **Plan Update Handler** (`acp_events.rs`) — 解析 JSON `entries` 数组 → 写入 `TODO_ITEMS` atom
9. **MessageArea 渲染** (`message_area.rs`) — 读取 `TODO_ITEMS` atom → `render_todo_lines()`

### Plan JSON 结构（来自 ACP SDK）

```json
{
  "sessionUpdate": "plan",
  "entries": [
    {"content": "Fix bug", "status": "in_progress", "priority": "medium"}
  ]
}
```

- `PlanEntryStatus`: `"pending"`, `"in_progress"`, `"completed"`（snake_case）
- 序列化配置: `#[serde(rename_all = "camelCase")]`
- Plan 结构被包裹在 `{"session_id": "...", "update": <Plan>}` 中传输

### handle_plan_update 解析逻辑

- 从 `update.get("entries")` 获取数组
- 对每个 entry：直接取 `content` 字段，`status` 字段做字符串匹配
- status 匹配 `"in_progress"` / `"completed"` / `"pending"`，其他值 → `return None`（filter_map 丢弃）

## 可能根因方向

1. **JSON 序列化字段名不匹配**：Plan 经 `#[serde(rename_all = "camelCase")]` 序列化后字段为 camelCase，但 `handle_plan_update` 直接 `update.get("entries")`，若外层的 `"update"` 键名或 Plan 内部的字段名因 serde rename 与代码预期不同，会导致解析失败 → 空白
2. **Atom 更新竞争**：多次连续 TodoWrite 调用时，atom 写入的先后顺序可能不一致，最终可能写入了一个空列表或解析失败的默认值
3. **render_todo_lines 边界处理**：多条目渲染按条目逐一处理，理论上兼容多条，但不排除某些边界条件（如条目 key 冲突）导致跳过渲染

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-middlewares/src/tools/todo.rs` | TodoWrite 工具实现，通过 channel 通知 todo 更新 |
| `peri-middlewares/src/middleware/todo.rs` | TodoMiddleware，注册工具并持有 `notify_tx` |
| `peri-acp/src/agent/builder.rs` | 创建 `(todo_tx, todo_rx)` channel |
| `peri-acp/src/session/executor_helpers.rs` | Todo Forwarder，读取 `todo_rx` → 转换为 ExecutorEvent |
| `peri-acp/src/event/mapper.rs` | Event Mapper，`TodoUpdate` → `SessionUpdate::Plan` |
| `peri-acp/src/acp_client/client.rs` | ACP Client，转发 SessionUpdate |
| `peri-tui/src/kit/acp_notifier.rs` | 匹配 `tag == "plan"`，路由到 handler |
| `peri-tui/src/kit/acp_events.rs` | `handle_plan_update()`，解析 JSON → 写入 TODO_ITEMS atom |
| `peri-tui/src/kit/message_area.rs` | 消息区渲染，读取 TODO_ITEMS → `render_todo_lines()` |

## 复现步骤

1. 启动 TUI，连接 agent
2. 发送一条会触发多条目 TodoWrite 的 prompt（例如要求 agent 创建一个包含 3+ 步骤的任务计划）
3. 观察 TUI 底部 Todo 区域是否正常显示条目列表

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（待修复）
