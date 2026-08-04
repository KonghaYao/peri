# ACP / Session 领域

## 领域综述

Peri ACP 服务层：session 生命周期（`SessionManager`/`AcpSession`）、prompt 构建（`PromptTemplate` + 冻结数据）、Agent 与中间件装配（`build_agent`）、事件映射/发送（`map_event` → `EventSink`）、Langfuse 观测（`LangfuseBridge`）与 transport 传输（`AcpTransport`：mpsc/stdio）。`peri-acp/src/session/` 管 session 生命周期与事件/prompt，ReAct 循环本体在 `peri-agent`（`run_react_loop`）。

## 核心流程

- **请求入口**：`peri-tui/src/acp_server/mod.rs::run_acp_server` 接收 JSON-RPC 请求；`session/prompt` spawn 后台任务执行，保持 loop 响应 `session/cancel`
- **session 创建**：`SessionManager::new_session` / `new_session_with_settings` 建 `AcpSession`（句柄，核心状态委托 `peri_agent::session::Session`），同时 `FrozenSessionData::build` 冻结 CLAUDE.md/skills/system prompt/date；`new_session_with_id` 服务 `session/load` 与 `session/resume`
- **prompt 构建**：`PromptTemplate::render(&env, &features, …)` 从 `prompts/sections/`（01_intro…16_workflow）按 `FeatureGate`/`PromptFeatures` 条件注入；frozen 数据 session 中途不重读（ARC-FROZEN-001）
- **agent 执行**：`run_session_loop(SessionContext, TurnInput) -> PromptResult` 装配 v2 `StageContext` 并调用 `peri-agent` 的 ReAct 循环；slash 命令经 `session::command::CommandRegistry`（Immediate 命令也可由 `execute_command` 直接执行）
- **事件流**：v2 EventBus 三通道（render/state/observe）→ `spawn_eventbus_forwarder`（biased select，render 先于 state）→ `ExecutorEvent` → `map_event` → `AcpEvent` DTO（按 session caps 门控）→ `EventSink` → `session/update`（标准 ACP，带 `_peri` metadata）、`peri/agent_event`、`peri/unstable-event`
- **session/update 标准事件**：`dispatch/session_replay.rs::replay_session_history` 在 `session/load` 时通过 `session/update`（`user_message_chunk` 等）重放整个对话
- **交互桥**：`AcpTransportBroker` 将 HITL 审批转 `RequestPermission` RPC、AskUser 问题转 `elicitation/create` RPC
- **观测**：`LangfuseBridge.process_event`（`UnifiedLangfuseEvent` 单一入口）→ `LangfuseTracer`（Trace → Span → Generation），不改变客户端事件路径

## 技术方案总结

| 维度 | 选型 |
|------|------|
| session 模型 | `AcpSession`（ACP 侧句柄）+ `SessionManager`（DashMap 会话表 + caps registry）；v2 迁移后核心状态委托 `peri_agent::session::Session` |
| prompt 管线 | `PromptTemplate` 按 `FeatureGate` 条件注入 section；`PromptEnv` 注入 cwd/git/OS/date；session/new 时冻结，跨 turn 复用 |
| 事件系统 | v2 EventBus 三通道 → `spawn_eventbus_forwarder` → `ExecutorEvent` → `map_event` → `AcpEvent` DTO → `EventSink`；caps 门控扩展事件 |
| Langfuse | `LangfuseBridge` + `UnifiedLangfuseEvent` 统一映射 → `LangfuseTracer`（stages/generation/tool_batch/compact/subagent 等子模块） |
| transport/broker | `AcpTransport` trait（JSON-RPC 2.0）：`MpscClientTransport`/`MpscServerTransport`（TUI 内存通道）/ `StdioTransport`（IDE）；`RequestRouter` 共享 pending 请求表；`AcpTransportBroker` 桥接 HITL/AskUser |
| provider | `LlmProvider`（OpenAi/Anthropic 枚举）→ `peri-model` LLM 工厂；`PeriConfig`/`AppConfig` 配置与 store 持久化 |

## 稳定入口

| 模块 | 职责 |
|------|------|
| `peri-acp/src/session/mod.rs` | `SessionManager`（new/load/resume/fork/close/cancel/list、`ensure_session_caps`、`build_frozen_data`）、`AcpSession` |
| `peri-acp/src/session/executor.rs` | `run_session_loop`（主执行入口）、`FrozenSessionData::build`、`is_keepgoing`（空白 prompt = 继续跑 loop）、`execute_prediction` |
| `peri-acp/src/session/event_sink.rs` | `EventSink` trait / `TransportEventSink`（session/update、peri/agent_event、peri/unstable-event 三通道） |
| `peri-acp/src/agent/builder.rs` | `build_agent`、`AgentComponents`（中间件链 + LLM + system prompt）、`FrozenData`；生产中间件顺序事实源 |
| `peri-acp/src/prompt/mod.rs` | `PromptTemplate`、`PromptFeatures::detect`、`PromptEnv`；prompt section 可见性条件源 |
| `peri-acp/src/event/` | `mapper.rs::map_event` → `MappedEvent`/`AcpEvent`；`forwarder.rs::spawn_eventbus_forwarder`（v2 → ExecutorEvent） |
| `peri-acp/src/langfuse/` | `bridge.rs::LangfuseBridge`（统一入口）、`tracer.rs::LangfuseTracer`、`config.rs::LangfuseConfig` |
| `peri-acp/src/transport/` | `AcpTransport` trait、`mpsc.rs`（MpscClientTransport/MpscServerTransport）、`stdio.rs`（StdioTransport）、`router.rs`（RequestRouter） |
| `peri-acp/src/broker/transport_broker.rs` | `AcpTransportBroker`（HITL/AskUser → ACP RPC） |
| `peri-acp/src/dispatch/` | `handle_prompt`、`execute_command`、`fork_session`、`load_session_messages`、`replay_session_history`、`rewind_execute`、`build_initialize_response`（TUI/stdio 共享业务逻辑） |
| `peri-acp/src/provider/` | `LlmProvider`、`PeriConfig`/`AppConfig`、`store.rs` 配置读写 |
| `peri-acp/src/session/command/` | `CommandRegistry`、`default_command_registry`（bg/clear/compact/rewind 等） |
| 边界 | ACP 管 session 生命周期/事件/prompt；`peri-agent/src/session/`（`Session`、`MessageQueue`、`FrozenContext`）与 `run_react_loop` 管 ReAct 循环与执行状态 |

## Issue 经验附录

相关历史 issue 见 `domains/agent.md`，本附录暂不迁移条目；后续 ACP/Session 领域 issue 归档于此。
