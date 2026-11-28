# 权威设计索引

`docs/design/` 只保存已批准的架构与产品设计。它回答“系统应当如何工作”，不记录
某次调查、迁移批次、实施日志、完成清单或个人复盘。

现行行为由代码、契约测试和复现证明；工程规则由 `docs/standards/` 维护，已批准的
目标由权威设计约束。按 `STD-INDEX-002` 区分实现缺陷、文档过时和获批契约变更，
不能以旧实现或旧测试降低已批准目标，也不能把目标描述成已经实现。

## 状态含义

- **现行设计**：描述已落地的稳定结构，不等于发布、端到端或规模验收；具体符号和文件入口以代码索引与源码为准。
- **已批准目标设计**：描述已经裁决但尚未完全落地的目标；实现进度、临时风险和
  验收勾选只写对应 `spec/issues/`。
- **待重构**：已批准目标设计中设计已收敛、实现差距已审计的部分；作为代码重构基线，
  范围与顺序见对应 active issue。

draft、proposal、可行性探查、审计报告和未采纳方案不进入本目录。需要长期保留的
外部生态或操作资料放 `docs/reference/`；过程由 active issue 承载，完整历史由 Git
保留。

## 现行设计

| 主题 | 文档 | 边界 |
| --- | --- | --- |
| 总体分层 | [architecture.md](architecture.md) | crate 职责、依赖方向与跨层数据流 |
| 配置权威面 | [configuration-authority.md](configuration-authority.md) | Peri source adapters、纯 typed 规则、scoped snapshot/revision、解释与 CAS 更新；专属领域仍按能力边界扩展 |
| ACP wire | [peri-acp-protocol.md](peri-acp-protocol.md) | 方法、事件、transport 与兼容语义 |
| ACP v2 stdio Preview | [acp-v2-stdio-preview.md](acp-v2-stdio-preview.md) | 外部 stdio 协商、兼容边界与验证入口 |
| Model adapter | [model-adapters.md](model-adapters.md) | provider 无关协议、stream、retry 与观测 |
| System Prompt | [system-prompt.md](system-prompt.md) | 冻结 base、request-time contribution 与 cache seam |
| MetaHarness | [meta-harness.md](meta-harness.md) | 段落覆盖、middleware 关闭与冻结语义 |
| ACP MCP 运行控制 | [acp-mcp-control.md](acp-mcp-control.md) | ACP 按会话环境注册/重启/关闭普通 MCP，TUI 提供重启/关闭；临时状态与调用、发现及清理生命周期 |
| MCP 响应缓存 | [mcp-cache.md](mcp-cache.md) | 文件/环境总开关、pool 策略与持久化/SDK 缓存关闭闭包 |
| Middleware | [middleware-system.md](middleware-system.md) | 生产链、hook、工具与 prompt contribution |
| 工具系统 | [tool-system.md](tool-system.md) | session-local 可见性、ToolSearch 与执行边界 |
| 交互 broker | [interaction-brokers.md](interaction-brokers.md) | Approval/Questions broker 与 transport 交互 |
| 消息存储 | [message-transcript.md](message-transcript.md) | Transcript、MessageQueue、staging 与持久化 |
| 会话身份、工作区归属与恢复 | [session-id-environment.md](session-id-environment.md) | Session ID 与机器归属、工作区发现与登记、执行绑定与恢复入口 |
| 用户待发送队列 | [user-input-queue.md](user-input-queue.md) | Mailbox、单条/全部投递、取回与运行身份 |
| Session 异步任务统一入口 | [session-async-tasks.md](session-async-tasks.md) | 当前会话任务归属、结果/取消/关闭；**不恢复旧任务目录或执行身份**，消息与执行服从 RCRA 权威 |
| SubAgent 活动行 | [tui-subagent-activity.md](tui-subagent-activity.md) | 子 Agent 活动数据源、状态聚合与行投影 |
| Compact | [micro-compact.md](micro-compact.md) | Step/交换投影、原文回查、原子提交与 Full/fork/rewind 生命周期；门控和能力边界 |
| MCP Apps relay | [mcp-multiplexing.md](mcp-multiplexing.md) | stdio Apps profile、附件与记录绑定、多路数据隔离 |
| Meta 数据访问 | [meta-control.md](meta-control.md) | 只读 session metadata CLI、machines 目录查询与持久化边界 |
| Workflow | [workflow.md](workflow.md) | Node wire、runner、任务与进度、journal/state、resume/kill、TUI 投影与 CLI/artifact 分发 |
| Workflow MCP 接入 | [workflow-mcp.md](workflow-mcp.md) | 默认内置 `peri` 出口下的 Workflow 能力、任务与结果回填；旧独立实例仅用于回滚 |
| Ultra-ADLC | [ultra-adlc.md](ultra-adlc.md) | 超大交付模式的文件协议与编排契约 |
| TUI 数据流 | [tui-acp-data-flow.md](tui-acp-data-flow.md) | ACP event → Atom → render 链路 |
| TUI 流式 Markdown 性能 | [tui-streaming-markdown-performance.md](tui-streaming-markdown-performance.md) | publication scheduler、lazy projection、增量 Markdown 与 slot index |
| Git ref 观察 | [git-watch-middleware.md](git-watch-middleware.md) | 分支/HEAD 变化 Info 注入（异步采样 + 60s 节流） |
| System Reminder | [system-reminder.md](system-reminder.md) | canonical DTO、可信生产、队列/持久化、ACP/TUI 投影与 legacy fallback |
| Beta Flag 系统 | [beta-flags.md](beta-flags.md) | 注册表、`config.betas` 配置面投影与装配期消费注入；不做远程下发、灰度与会话内热生效，安全与兼容语义不使用 flag；验收记录与剩余跟进项见 [2026-10-08 实施 issue](../../spec/issues/2026-10-08-beta-flags-implementation.md) |
| Peri SDK Chat | [peri-sdk-chat.md](peri-sdk-chat.md) | `ChatManager`/`ChatService`/`ChatUserAction`/`PeriInstance`/`ChatView` 与 `ChatReplica` 的职责、事实/同步、宿主与关闭边界；公开入口见 [TS SDK 索引](../code-index/peri-ts-sdk.md) |
| Swarm 管理 TUI | [peri-swarm-dashboard.md](peri-swarm-dashboard.md) | 单网关 HTTP 客户端的终端管理面：凭据引导、会话日志/详情与动作、设置、Observability 配置与 Core/Agent/Request/Query 观测页；13 页中 Sandbox metrics 与资源类页面为有意空页 |

## 已批准目标设计

| 主题 | 文档 | 进度事实源 |
| --- | --- | --- |
| MCP 适配 v4-part-1 | [mcp-adaptation-v4-part-1.md](mcp-adaptation-v4-part-1.md) | System MCP 启动依赖、工具注入、MCP 隔离与 middleware 归属；builtin 实例最小隔离单位条款已被 [v5 单出口设计](mcp-v5-single-exit.md) 取代，六实例仅保留为兼容期路径；迁移状态以代码与契约测试为准 |
| MCP v5 单出口 | [mcp-v5-single-exit.md](mcp-v5-single-exit.md) | builtin 单 `peri` 出口、capability router、workspace 代理、能力关闭矩阵与迁移回滚契约；`peri` 已是宿主默认装配，旧 policy 键保留为确定性映射 adapter（M4 删除），`PERI_MCP_BUILTIN=legacy` 保留六实例回滚到 M4 截止决定；Sandbox workspace 代理接线与端到端验收见 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md) |
| 时间能力边界 | [time-runtime.md](time-runtime.md) | 原生边界已落地；入口见 [peri-time 索引](../code-index/peri-time.md) |
| RCRA 消息、任务归属与激活 | [rcra-message-activation.md](rcra-message-activation.md) | 当前进程 Task/执行、canonical 历史与普通执行；待发送消息持久化见 [Thread MQ](thread-mq.md)；恢复只按意图建新运行，不接管旧执行 |
| Session Durability | [session-durability.md](session-durability.md) | 进入会话时按持久工作意图建立新运行；不恢复旧执行身份；后端能力与验收缺口见本文 |
| Thread MQ 持久收件箱 | [thread-mq.md](thread-mq.md) | 主、子与 Workflow Agent 的统一持久 MQ、原子 Receive 与 Rust 操作接口；实施与验收见 [active issue](../../spec/issues/2026-11-01-thread-mq-implementation.md) |
| 存储 v2：Machine → Workspace → Session | [storage-v2-machine-workspace-session.md](storage-v2-machine-workspace-session.md) | schema 版本以代码索引为准，Git 使用 worktree 根、非 Git 使用启动 cwd；真实远端验收待执行 |
| Command 系统 | [command-system.md](command-system.md) | 命令协议与执行边界以代码和契约测试为准 |
| TUI Chat Workbench | [tui-chat-workbench.md](tui-chat-workbench.md) | transcript 展示、组合器与工作台布局的现行约束 |
| MonitorMCP | [monitor-mcp.md](monitor-mcp.md) | v1 command source、标准 MCP Tasks 与 alert 提醒已实现（运行中命中与任务终态都会唤醒，终态合并未报告命中）；实现入口见 [MCP packages 索引](../code-index/mcp-packages.md)，poll/file/WS source 不在范围 |
| Serverless 执行环境身份 | [serverless-execution-identity.md](serverless-execution-identity.md) | 历史设计（Emscripten/WASM 部署已移除）；保留移除前的目标设计与验收边界 |
| TUI 面板架构 | [tui-panel-architecture.md](tui-panel-architecture.md) | [TUI 面板架构重构](../../spec/issues/2026-10-15-tui-architecture-refactor.md)；面板外壳、共享视口、事件入口、操作作用域与完成门、快照发布单点、确认版本守卫已落地；未完成项见该 issue |
| Swarm 事件日志粒度 | [peri-swarm.md §9](peri-swarm.md#9-已批准目标事件日志粒度与回放置位已实施) | 文本/推理按 item 合并落库、`tool`/`approval` 保持逐帧、实时 delta 帧不带 seq、回放真源固定 PostgreSQL 且不引入 Redis；已实施，验收见 [active spec](../../spec/issues/2026-11-26-swarm-event-log-granularity.md) |
| Peri Swarm 本地开发宿主 | [peri-swarm.md](peri-swarm.md) | 五路由 HTTP 面、Rivet actor、Peri 会话库与独立 Swarm 状态库（均为 PostgreSQL）、Bun 沙箱与 MCP 注入、仅杀 Rust 的 reattach；副本组与外部 engine 配置（默认 loopback，显式非 loopback 开关）；本地验收见[验收记录](../verification/peri-swarm-local-dev.md)与[状态库迁移](../verification/peri-swarm-postgres-state-2026-11-24.md)，跨机、TLS、单 engine 多副本与规模未验收 |

## 维护要求

1. 新设计先确定状态、scope、代码事实源和对应 active issue。
2. 实施完成后删掉 issue 进度叙事，只把稳定结果同步为“现行设计”。
3. 文件改名、合并或删除时，同步根/模块路由、standards、code-index 与 active spec
   引用，并运行本地链接检查。
4. 动态 inventory、固定源码行号和命令输出不复制进设计；定位信息放
   `docs/code-index/`。

## 专用装配与部署设计

| 主题 | 唯一维护位置 | 边界 |
| --- | --- | --- |
| SDK 部署归属 | [OWNERSHIP-MODEL](../../npm-packages/@peri-sdk/OWNERSHIP-MODEL.md) | 使用方 User/Workspace/SessionDB 约束；不替代 SDK Chat 运行设计 |
| Rivet 对接与生产主线 | [Peri Swarm 设计](peri-swarm.md) | actor 接线、活动投影与资源生命周期；Rust 实现为唯一主线，无第三运行时 |
| Sandbox 实例 | [Sandbox 设计](../../npm-packages/peri-sandbox/peri-sandbox-design.md) | 单实例 HTTP MCP 与宿主资源管理；不承诺多租户隔离 |

这些包内设计继续原位维护，本索引只路由；不复制另一份 owner 或控制表。
试验性 TS 适配层 `npm-packages/peri-rivet` 与 `npm-packages/agent-builder` 已于
2026-11-22 移除；Rivet 对接与部署归属以 [Peri Swarm 设计](peri-swarm.md) 为准。
`peri-cf` 已退役，不能作为当前部署入口；历史研究不构成部署验收。
