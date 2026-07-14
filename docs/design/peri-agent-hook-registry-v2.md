# peri-agent v2 Hook 注册表面板

> 全部 hook 点的唯一真源 | 日期：2026-06-24 | 修订：v1.0

## 1. Hook 清单

| # | Hook | 触发阶段 | 消费方 | 说明 |
|---|------|---------|--------|------|
| 1 | `on_session_start` | Session 创建 | 中间件切面 | Agent 初始化完成后，ReAct 循环启动前 |
| 2 | `before_agent` | Agent 初始化 | 中间件切面 | 注入上下文（CLAUDE.md、Skills）、注册工具 |
| 3 | `before_model` | 每轮 LLM 前 | 中间件切面 | Token 检查、goal steering、上下文准备 |
| 4 | `after_model` | LLM 返回后 | 中间件切面 | 响应后处理、Token 累积校验 |
| 5 | `before_tool` | 单工具调用前 | 中间件切面 | 参数注入、权限检查 |
| 6 | `after_tool` | 单工具调用后 | 中间件切面 | 日志记录、结果转换 |
| 7 | `before_tools_batch` | 批量工具调用前 | 中间件切面 | 批量审批（HITL） |
| 8 | `after_tools_batch` | 批量结果写入后 | 中间件切面 | 聚合检查、持久化写入 |
| 9 | `after_agent` | Agent 完成后 | 中间件切面 | 输出后处理、资源清理 |
| 10 | `on_error` | 出错时 | 中间件切面 | 错误记录、告警 |
| 11 | `before_compact` | Compact 启动前 | 观测层 | 外部监听压缩开始 |
| 12 | `after_compact` | Compact 完成后 | 观测层 | 外部监听压缩结束 |
| 13 | `on_permission_request` | 权限审批请求时 | 观测层 | HITL 审批的观测事件 |
| 14 | `on_subagent_start` | SubAgent 启动时 | 观测层 | 子 Agent 生命周期追踪 |
| 15 | `on_subagent_stop` | SubAgent 结束时 | 观测层 | 子 Agent 生命周期追踪 |
| 16 | `on_turn_end` | 每轮 ReAct 结束时 | 观测层 | Turn 边界标记、Langfuse 上报 |
| 17 | `on_session_end` | Session 销毁时 | 观测层 | 资源释放、孤儿 Agent 清理 |

## 2. 分类

| 类别 | Hook 数量 | 列表 | 说明 |
|------|----------|------|------|
| **中间件切面** | 10 | #2-#9, #10 | 切面声明挂载，可修改 State |
| **观测层** | 5 | #11-#16 | 只读事件，不修改 State，external listener 消费 |
| **生命周期** | 2 | #1, #17 | Session 级边界钩子 |

### 2.1 before_tool vs before_tools_batch 执行顺序

两个 hook 面向不同粒度：

- **`before_tools_batch`**：批量级入口。先对所有工具调用执行 batch 逻辑（如 HITL 批量审批）。通过 batch 的工具进入下一阶段，被拒绝的直接返回错误。
- **`before_tool`**：逐工具级。对每个通过 batch 的工具单独执行（如参数注入、工具特定校验）。
- **执行顺序**：`before_tools_batch → before_tool（逐工具）→ 工具执行 → after_tool（逐工具）→ after_tools_batch`

不支持 batch 的切面在 batch 阶段自动退化为逐条调用。

## 3. 中间件切面的 hook 使用现状

| 切面 | 挂载的 hook |
|------|-----------|
| claude_md | before_agent |
| agent_define | before_agent |
| skills | before_agent |
| skill_preload | before_agent |
| at_mention | before_agent |
| user_hook | before_agent |
| agent_tool | before_agent |
| background_task | before_agent, after_agent |
| mcp_bridge | after_agent |
| hitl | before_tools_batch |
| goal_tracking | before_model |

## 4. ReAct 循环中的 hook 位置

```
session/new
  └─ on_session_start

execute_prompt:
  before_agent ───→ LLM call ───→ after_model
  ┌────────────────────┘              │
  │                            before_tools_batch
  │                                   │
  │                              tool execution (并发)
  │                                   │
  │                            after_tools_batch
  │                                   │
  └──────────── before_model ←────────┘  (next turn)

  最终答案
  after_agent
  on_turn_end
```

Compact 是 ReAct 循环的一等步骤（非 hook），位于 before_model 之前：

```
before_agent → Compact(条件性) → before_model → LLM → after_model → ...
```
