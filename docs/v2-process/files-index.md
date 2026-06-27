# v2 重做相关文件索引

> 按角色分类。接手者通读「v2 主路径」+「双轨边界」两组即可上手。

## v2 主路径（热路径）

### peri-agent（核心）

| 文件 | 角色 | 行数估算 |
|------|------|----------|
| `peri-agent/src/session/mod.rs` | `Session` 统一入口（聚合 store/queue/config/transcript/turn） | ~200 |
| `peri-agent/src/session/store.rs` | `SessionStore` + `FrozenContext` + `SessionId` | ~280 |
| `peri-agent/src/session/queue.rs` | v2 `MessageQueue`（`MessageKind::{Prompt,Defer,Info}`） | ~280 |
| `peri-agent/src/session/config.rs` | `SessionConfig` + `PermissionMode` + cancel token | ~180 |
| `peri-agent/src/session/transcript.rs` | `MessageTranscript`（id 索引 + 标记系统：truncated/excluded） | ~700 |
| `peri-agent/src/session/turn.rs` | `TurnContext`（`TurnId` + step + cancel_token） | ~160 |
| `peri-agent/src/group/mod.rs` + `pipeline.rs` | `AgentGroup` + `AgentPipeline` + `CancelPolicy` | ~520 |
| `peri-agent/src/agent/stages/mod.rs` | `StageContext`（12 字段）+ 5 阶段类型 + `run_react_loop` 编排器 | ~920 |
| `peri-agent/src/agent/stages/reason.rs` | `run_reason`（before_model + LLM + after_model） | ~200 |
| `peri-agent/src/agent/stages/act.rs` | `run_act`（3 阶段工具分发） | ~250 |
| `peri-agent/src/agent/stages/tool_dispatch.rs` | 延迟写入不变量 + error_suggest 注入 | ~400 |
| `peri-agent/src/agent/stages/compact.rs` | `run_compact`（disable 检查 + cancel select） | ~160 |
| `peri-agent/src/agent/stages/receive.rs` | `drain_for_receive` + Info 包裹 system-reminder | ~150 |
| `peri-agent/src/agent/stages/end.rs` | `drain_for_end` 唤醒逻辑 | ~120 |
| `peri-agent/src/agent/stages/middleware_runner.rs` | StageContext → MiddlewareContext 适配 | ~150 |
| `peri-agent/src/agent/compact_v2.rs` | v2 compact 实现（标记代替删除） | ~600 |
| `peri-agent/src/agent/events_v2.rs` | `EventBus` + `RenderEvent` / `StateEvent` / `ObserveEvent` | ~400 |
| `peri-agent/src/middleware/trait.rs` | `trait Middleware`（无泛型）+ 17 钩子 | ~200 |
| `peri-agent/src/middleware/chain.rs` | `MiddlewareChain`（16 个 run_* 方法） | ~400 |
| `peri-agent/src/middleware/context.rs` | `MiddlewareContext` / `Mut` / `Inner` | ~250 |

### peri-acp（ACP 服务层）

| 文件 | 角色 |
|------|------|
| `peri-acp/src/session/executor.rs` | `execute_prompt()` 入口 + `run_session_loop` + `build_and_execute_agent_v2`（9 Phase）+ `spawn_event_pump` |
| `peri-acp/src/agent/builder.rs` | v1 `build_agent()`（v2 仍复用装配） |
| `peri-acp/src/agent/builder_v2.rs` | `build_stage_context()`（P3.2 + Stage 1 Top 5 修复点） |
| `peri-acp/src/event/mapper_v2.rs` | `render_event_to_executor` / `state_event_to_executor` / `observe_event_to_executor` |
| `peri-acp/src/event/dto.rs` | DTO 层：`CompactFileInfoDto` / `WorkflowProgressDto` / `TokenUsageDto` / `StopReasonDto` / `TodoItemDto` / `TodoStatusDto` |
| `peri-acp/src/session/event_sink.rs` | `TransportEventSink` |
| `peri-acp/src/session/agent_pool.rs` | `AgentPool` + `CachedLlmInstances`（含 `auxiliary_model`） |

### peri-tui（前端）

| 文件 | 角色 |
|------|------|
| `peri-tui/src/app/chat_session.rs` | `ChatSession`（含 `message_queue: Arc<MessageQueue>`、`todo_items: Vec<TodoItemDto>`） |
| `peri-tui/src/app/agent_ops/acp_bridge.rs` | `AcpNotification → AgentEvent` 桥接（DTO 化） |
| `peri-tui/src/app/agent.rs` | `ExecutorEvent → AgentEvent` 映射（`map_executor_event`） |
| `peri-tui/src/acp_client/client.rs` | `AcpTuiClient`（MpscTransport 前端） |

---

## v1 残留（待 P5 物理删除）

| 文件 | 角色 | 删除前置 |
|------|------|----------|
| `peri-agent/src/agent/executor/mod.rs` | `ReActAgent::execute()` v1 热路径 | P5.1 + P5.2 + P5.4 |
| `peri-agent/src/agent/executor/tool_dispatch.rs` | v1 工具分发（与 stages/tool_dispatch.rs 重复） | P5.4 |
| `peri-agent/src/agent/executor/tool_setup.rs` | v1 工具注册 | P5.4 |
| `peri-agent/src/agent/executor/final_answer.rs` | v1 最终回答 | P5.4 |
| `peri-agent/src/agent/executor/llm_step.rs` | v1 LLM 步骤 | P5.4 |
| `peri-agent/src/agent/executor/mod_test.rs` | v1 executor 测试（91 个） | P5.4 |
| `peri-agent/src/agent/executor/tool_dispatch_test.rs` | v1 tool_dispatch 测试 | P5.4 |
| `peri-agent/src/agent/react.rs::ReActAgent` | v1 ReActAgent 块（保留 `Reasoning` / `ToolCall` / `ToolResult` 类型） | P5.5 |
| `peri-agent/src/agent/state.rs` | `State` trait + `AgentState` | P5.5（或收缩为测试辅助） |
| `peri-agent/src/agent/events.rs` | v1 `AgentEvent` 枚举 | P5.5 |
| `peri-agent/src/messages/message_queue.rs` | v1 `MessageQueue`（v2 已有 `session/queue.rs`） | P5.5 |
| `peri-agent/src/agent/compact/` | v1 compact 实现（v2 已有 `compact_v2.rs`） | P5.5 |

---

## 双轨边界（v1 ↔ v2 桥接）

### builder_v2（v2 调用 v1 装配）
- `peri-acp/src/agent/builder_v2.rs::build_stage_context` —— 包装 v1 `build_agent`，通过 `into_parts()` 拆解

### SubAgent（v1 路径）
- `peri-middlewares/src/subagent/mod.rs` —— `SubAgentMiddleware`
- `peri-middlewares/src/subagent/tool/define.rs`
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— 后台 Fork
- `peri-middlewares/src/subagent/tool/execute_fork.rs` —— 同步 Fork

### Hook（v1 路径）
- `peri-middlewares/src/hooks/middleware.rs` —— `HookMiddleware`
- `peri-middlewares/src/hooks/executor.rs` —— Hook agent 执行

### EventBus forwarder（v2 事件 → v1 ExecutorEvent）
- `peri-acp/src/session/executor.rs` Phase 4（行 ~1325–1376）—— `tokio::select!` 排空三层通道

### CompactMiddleware（v1 入口，v2 stage 入口并行）
- `peri-middlewares/src/compact_middleware.rs` —— v1 `before_model` 钩子触发 compact
- `peri-agent/src/agent/stages/compact.rs` —— v2 stage 入口触发 compact

**注意**：v2 路径下，CompactMiddleware 仍会运行（在 chain 中），但其 `before_model` 中的 compact 触发逻辑应该被 v2 stage 接管。需要审计 CompactMiddleware 在 v2 路径下是否真的会触发（若会，会与 stages/compact.rs 重复）。

---

## 修复点索引（Stage 1/2/3）

| Stage | Top | 文件 | 行 |
|-------|-----|------|-----|
| 1 ✅ | Top 1 | `peri-acp/src/session/executor.rs` | ~1325–1376 |
| 1 ✅ | Top 4 | `peri-agent/src/agent/stages/compact.rs` | ~30–39 |
| 1 ✅ | Top 5 | `peri-acp/src/agent/builder_v2.rs` | ~65–71, ~136–138 |
| 1 ✅ | Top 8 | `peri-agent/src/agent/stages/compact.rs` | ~70–90 |
| 2 ⏳ | Top 2 | `peri-acp/src/agent/builder_v2.rs` + `peri-agent/src/session/mod.rs` | TBD |
| 2 ⏳ | Top 3 | `peri-agent/src/session/transcript.rs` + `compact_v2.rs` | TBD |
| 2 ⏳ | Top 6 | `peri-acp/src/event/mapper_v2.rs` | TBD |
| 2 ⏳ | Top 7 | `peri-acp/src/session/executor.rs` Phase 5 | TBD |
| 3 ⏳ | Top 9 | `peri-agent/src/agent/stages/act.rs` | TBD |
| 3 ⏳ | Top 10 | `peri-agent/src/agent/stages/reason.rs` | TBD |

---

## 文档与历史

| 文件 | 角色 |
|------|------|
| `CLAUDE.md` | 项目工作守则（v2 架构状态段落需随进度更新） |
| `~/.claude/plans/majestic-zooming-haven.md` | P1–P5 原始计划 |
| `docs/superpowers/plans/2026-06-24-v2-architecture-status.md` | 6-24 历史快照 |
| `docs/design/peri-agent-*.md` | v2 设计文档（10 份） |
| `docs/v2-process/README.md` | 本目录入口 |
| `docs/v2-process/2026-06-25-stage1-complete.md` | 最新快照 |
| `docs/v2-process/roadmap.md` | 剩余路线 |
| `docs/v2-process/files-index.md` | 本文件 |
| `docs/v2-process/verification.md` | 验证步骤 |
