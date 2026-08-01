# peri-agent v2 Hook 注册表面板

> 全部 hook 点的唯一真源 | 日期：2026-07-15 | 修订：v1.1

## 1. Hook 清单

| # | Hook | 触发阶段 | 消费方 | 说明 |
|---|------|---------|--------|------|
| 1 | `on_session_start` | Session 创建 | 生命周期 | Agent 初始化完成后，ReAct 循环启动前 |
| 2 | `on_user_prompt` | 用户提交 prompt | 中间件切面 | prompt 预处理、意图识别、上下文注入 |
| 3 | `before_agent` | Agent 初始化 | 中间件切面 | 注入上下文（CLAUDE.md、Skills）、注册工具 |
| 4 | `before_model` | 每轮 LLM 前 | 中间件切面 | Token 检查、goal steering、上下文准备 |
| 5 | `after_model` | LLM 返回后 | 中间件切面 | 响应后处理、Token 累积校验 |
| 6 | `before_tool` | 单工具调用前 | 中间件切面 | 参数注入、权限检查 |
| 7 | `after_tool` | 单工具调用后 | 中间件切面 | 日志记录、结果转换 |
| 8 | `before_tools_batch` | 批量工具调用前 | 中间件切面 | 批量审批（HITL） |
| 9 | `after_tools_batch` | 批量结果写入后 | 中间件切面 | 聚合检查、持久化写入 |
| 10 | `after_agent` | Agent 完成后 | 中间件切面 | 输出后处理、资源清理 |
| 11 | `on_error` | 出错时 | 中间件切面 | 错误记录、告警 |
| 12 | `before_compact` | Compact 启动前 | 观测层 | 外部监听压缩开始 |
| 13 | `after_compact` | Compact 完成后 | 观测层 | 外部监听压缩结束 |
| 14 | `on_permission_request` | 权限审批请求时 | 观测层 | HITL 审批的观测事件 |
| 15 | `on_subagent_start` | SubAgent 启动时 | 观测层 | 子 Agent 生命周期追踪 |
| 16 | `on_subagent_stop` | SubAgent 结束时 | 观测层 | 子 Agent 生命周期追踪 |
| 17 | `on_turn_end` | 每轮 ReAct 结束时 | 观测层 | Turn 边界标记、Langfuse 上报 |
| 18 | `on_notification` | 通知事件 | 观测层 | 外部通知、系统消息桥接 |
| 19 | `on_session_end` | Session 销毁时 | 生命周期 | 资源释放、孤儿 Agent 清理 |

## 2. 分类

| 类别 | Hook 数量 | 列表 | 说明 |
|------|----------|------|------|
| **中间件切面** | 10 | #2-#11 | 切面声明挂载，可修改 State |
| **观测层** | 7 | #12-#18 | 只读事件，不修改 State，external listener 消费 |
| **生命周期** | 2 | #1, #19 | Session 级边界钩子 |

### 2.1 before_tool vs before_tools_batch 执行顺序

两个 hook 面向不同粒度：

- **`before_tools_batch`**：批量级入口。先对所有工具调用执行 batch 逻辑（如 HITL 批量审批）。通过 batch 的工具进入下一阶段，被拒绝的直接返回错误。
- **`before_tool`**：逐工具级。对每个通过 batch 的工具单独执行（如参数注入、工具特定校验）。
- **执行顺序**：`before_tools_batch → before_tool（逐工具）→ 工具执行 → after_tool（逐工具）→ after_tools_batch`

不支持 batch 的切面在 batch 阶段自动退化为逐条调用。

## 3. 中间件切面的 hook 使用现状

| 切面 | 挂载的 hook |
|------|-----------|
| AgentsMdMiddleware | before_agent（CLAUDE.md/AGENTS.md 注入） |
| AgentDefineMiddleware | before_agent（agent 定义，model/maxTurns 覆盖） |
| PluginMiddleware | before_agent（插件兼容性校验、加载状态门控） |
| SkillsMiddleware | before_agent（Skills 摘要注入） |
| SkillPreloadMiddleware | before_agent（#skill-name 全文注入） |
| AtMentionMiddleware | before_agent（@path 解析） |
| HookMiddleware | before_agent（hooks 事件拦截，多组实例） |
| GitAttributionMiddleware | before_agent, before_tool, after_tool（Write/Edit 贡献字符数追踪） |
| McpMiddleware | collect_tools（MCP 工具和资源注入，无 async hook） |
| WorkflowMiddleware | before_agent（条件注册，WorkflowTool 为 deferred tool） |
| ToolSearchMiddleware | before_agent（SearchExtraTools/ExecuteExtraTool 代理） |
| HumanInTheLoopMiddleware | before_tools_batch, before_tool（HITL 审批） |
| SubAgentMiddleware | before_agent（SubAgent 工具注册） |
| GoalMiddleware | after_agent（递增紧迫感 steering + block_continue 自驱续跑，链最后） |
| LspMiddleware | after_tool（条件注册，LSP 文件变更同步） |

### 3.1 声明式 Prompt 贡献机制

除 async hook 外，`Middleware` trait 还提供 `prompt_contribution() -> Option<String>` 方法（见 `peri-agent/src/middleware/trait.rs:168`）。中间件通过此方法声明对 System Prompt 的文本贡献。

**收集路径**：`MiddlewareChain::collect_prompt_contributions()`（`chain.rs:312`）按注册顺序收集所有中间件的贡献，拼接为单个 String。

**合并点**：`peri-acp/src/agent/builder.rs:655` 在链构造完成后、构造 `AgentModelBridge` 前，将贡献拼接到 frozen system prompt 之后（`format!("{system_prompt}\n\n{contributions}")`）。

**设计意图**：保持 prompt cache 前缀稳定（不再通过 `prepend_message` 注入）。贡献中间件包括 AgentsMd / Skills / GitAttribution / ToolSearch 等。

### 3.2 middleware_runner 桥接层

v2 stages 不直接调 `MiddlewareChain` 的 `run_*` 方法，而是通过 `peri-agent/src/agent/stages/middleware_runner.rs` 桥接。该模块提供辅助函数（`run_before_compact`/`run_after_compact` 等），内部将 `StageContext` 转换为 `&mut dyn MiddlewareState` 后委托给 chain。

**Compact hook 调用路径**：`stages/compact.rs:26` 调 `middleware_runner::run_before_compact`，`:187` 调 `run_after_compact`——而非直接调 chain。这确保了 compact 作为 ReAct 一等步骤的同时，仍能触发中间件的 before_compact/after_compact hook。

## 4. ReAct 循环中的 hook 位置

```
session/new
  └─ on_session_start

execute_prompt:
  on_user_prompt ──→ before_agent ───→ LLM call ───→ after_model
                     ┌─────────────────────┘              │
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
  on_notification (外部事件随时注入)
```

Compact 是 ReAct 循环的一等步骤（非 hook），位于 before_model 之前。
compact 内部通过 `middleware_runner` 桥接层触发 `before_compact`/`after_compact` hook（见 §3.2）：

```
before_agent → Compact(条件性) → before_model → LLM → after_model → ...
               ├─ middleware_runner::run_before_compact
               └─ middleware_runner::run_after_compact
```
