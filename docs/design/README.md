# 权威设计索引

`docs/design/` 只保存已批准的架构与产品设计。它回答“系统应当如何工作”，不记录
某次调查、迁移批次、实施日志、完成清单或个人复盘。

优先级仍是：代码与契约测试 > `docs/standards/` > 模块 `CLAUDE.md` >
`docs/design/` > active spec > history。设计与更高优先级事实冲突时，在同一变更中
修正文档；不能用设计覆盖已验证行为。

## 状态含义

- **现行设计**：描述已落地的稳定结构；具体符号和文件入口以代码索引与源码为准。
- **已批准目标设计**：描述已经裁决但尚未完全落地的目标；实现进度、临时风险和
  验收勾选只写对应 `spec/issues/`。

draft、proposal、可行性探查、审计报告和未采纳方案不进入本目录。需要长期保留的
外部生态或操作资料放 `docs/reference/`；过程由 active issue 承载，完整历史由 Git
保留。

## 现行设计

| 主题 | 文档 | 边界 |
| --- | --- | --- |
| 总体分层 | [architecture.md](architecture.md) | crate 职责、依赖方向与跨层数据流 |
| 配置权威面 | [configuration-authority.md](configuration-authority.md) | Peri source adapters、纯 typed 规则、scoped snapshot/revision、解释与 CAS 更新；专属领域仍按能力边界扩展 |
| ACP wire | [peri-acp-protocol.md](peri-acp-protocol.md) | 方法、事件、transport 与兼容语义 |
| Model adapter | [model-adapters.md](model-adapters.md) | provider 无关协议、stream、retry 与观测 |
| System Prompt | [system-prompt.md](system-prompt.md) | 冻结 base、request-time contribution 与 cache seam |
| MetaHarness | [meta-harness.md](meta-harness.md) | 段落覆盖、middleware 关闭与冻结语义 |
| MCP 响应缓存 | [mcp-cache.md](mcp-cache.md) | 文件/环境总开关、pool 策略与持久化/SDK 缓存关闭闭包 |
| Middleware | [middleware-system.md](middleware-system.md) | 生产链、hook、工具与 prompt contribution |
| 工具系统 | [tool-system.md](tool-system.md) | session-local 可见性、ToolSearch 与执行边界 |
| 交互 broker | [interaction-brokers.md](interaction-brokers.md) | Approval/Questions broker 与 transport 交互 |
| 消息存储 | [message-transcript.md](message-transcript.md) | Transcript、MessageQueue、staging 与持久化 |
| 会话身份、工作区归属与恢复 | [session-id-environment.md](session-id-environment.md) | Session ID 与机器归属、工作区发现与登记、执行绑定与恢复入口；进度与验收见 [核心改动清单](../../spec/issues/2026-09-30-session-id-environment-core-change.md) |
| 用户待发送队列 | [user-input-queue.md](user-input-queue.md) | Mailbox、单条/全部投递、取回与运行身份 |
| Compact | [micro-compact.md](micro-compact.md) | 压缩计划与 LLM projection |
| Dynamic MCP | [dynamic-mcp.md](dynamic-mcp.md) | session 动态加载、目录发布与关闭 |
| MCP 适配 v4-part-1 | [mcp-adaptation-v4-part-1.md](mcp-adaptation-v4-part-1.md) | System MCP 启动依赖、工具注入、MCP 隔离与 middleware 归属 |
| MCP Apps relay | [mcp-multiplexing.md](mcp-multiplexing.md) | stdio Apps profile、binding lease 与多路数据隔离 |
| Meta 数据访问 | [meta-control.md](meta-control.md) | 只读 session metadata CLI 与持久化边界 |
| Workflow | [workflow.md](workflow.md) | Node RPC、runner、通知、kill 与 resume |
| Ultra-ADLC | [ultra-adlc.md](ultra-adlc.md) | 超大交付模式的文件协议与编排契约 |
| TUI 数据流 | [tui-acp-data-flow.md](tui-acp-data-flow.md) | ACP event → Atom → render 链路 |
| TUI 流式 Markdown 性能 | [tui-streaming-markdown-performance.md](tui-streaming-markdown-performance.md) | publication scheduler、lazy projection、增量 Markdown 与 slot index |
| Git Watch 中间件 | [git-watch-middleware.md](git-watch-middleware.md) | 分支/HEAD 变化 Info 注入（异步采样 + 60s 节流） |
| System Reminder | [system-reminder.md](system-reminder.md) | canonical DTO、可信生产、队列/持久化、ACP/TUI 投影与 legacy fallback |

## 已批准目标设计

| 主题 | 文档 | 进度事实源 |
| --- | --- | --- |
| Session 异步任务统一入口 | [session-async-tasks.md](session-async-tasks.md) | 任务投影、Store 执行代际、Workspace fencing 与可信关闭接管边界见设计第 5–6 节 |
| 存储 v2：Machine → Workspace → Session | [storage-v2-machine-workspace-session.md](storage-v2-machine-workspace-session.md) | `spec/issues/2026-10-02-storage-v2-machine-workspace-session.md`；schema 版本以代码索引为准，Git 使用 worktree 根、非 Git 使用启动 cwd；验收状态见 active issue |
| Command 系统 | [command-system.md](command-system.md) | 对应 command active issue 与代码 |
| TUI Chat Workbench | [tui-chat-workbench.md](tui-chat-workbench.md) | `spec/history/2026-08.md`（2026-08-10 条目） |
| SubAgent 活动行 | [tui-subagent-activity.md](tui-subagent-activity.md) | TUI redesign active issue |

## 维护要求

1. 新设计先确定状态、scope、代码事实源和对应 active issue。
2. 实施完成后删掉 issue 进度叙事，只把稳定结果同步为“现行设计”。
3. 文件改名、合并或删除时，同步根/模块路由、standards、code-index 与 active spec
   引用，并运行本地链接检查。
4. 动态 inventory、固定源码行号和命令输出不复制进设计；定位信息放
   `docs/code-index/`。
