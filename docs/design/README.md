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
外部生态或操作资料放 `docs/reference/`；过程与历史由 active/archive issue 和 Git
历史承载。

## 现行设计

| 主题 | 文档 | 边界 |
| --- | --- | --- |
| 总体分层 | [architecture.md](architecture.md) | crate 职责、依赖方向与跨层数据流 |
| ACP wire | [peri-acp-protocol.md](peri-acp-protocol.md) | 方法、事件、transport 与兼容语义 |
| Model adapter | [model-adapters.md](model-adapters.md) | provider 无关协议、stream、retry 与观测 |
| System Prompt | [system-prompt.md](system-prompt.md) | 冻结 base、request-time contribution 与 cache seam |
| MetaHarness | [meta-harness.md](meta-harness.md) | 段落覆盖、middleware 关闭与冻结语义 |
| Middleware | [middleware-system.md](middleware-system.md) | 生产链、hook、工具与 prompt contribution |
| 工具系统 | [tool-system.md](tool-system.md) | session-local 可见性、ToolSearch 与执行边界 |
| PTC | [programmatic-tool-calling.md](programmatic-tool-calling.md) | JavaScript host、RPC 与 effective tool dispatch |
| 交互 broker | [interaction-brokers.md](interaction-brokers.md) | Approval/Questions broker 与多路审批 |
| 消息存储 | [message-transcript.md](message-transcript.md) | Transcript、MessageQueue、staging 与持久化 |
| Compact | [micro-compact.md](micro-compact.md) | 压缩计划与 LLM projection |
| Dynamic MCP | [dynamic-mcp.md](dynamic-mcp.md) | session 动态加载、目录发布与关闭 |
| Workflow | [workflow.md](workflow.md) | Node RPC、runner、通知、kill 与 resume |
| Ultra-ADLC | [ultra-adlc.md](ultra-adlc.md) | 超大交付模式的文件协议与编排契约 |
| TUI 数据流 | [tui-acp-data-flow.md](tui-acp-data-flow.md) | ACP event → Atom → render 链路 |

## 已批准目标设计

| 主题 | 文档 | 进度事实源 |
| --- | --- | --- |
| Command 系统 | [command-system.md](command-system.md) | 对应 command active issue 与代码 |
| MCP Apps relay | [mcp-multiplexing.md](mcp-multiplexing.md) | `spec/issues/2026-08-27-mcp-apps-stdio-relay.md` |
| TUI Chat Workbench | [tui-chat-workbench.md](tui-chat-workbench.md) | `spec/issues/2026-08-10-chat-redesign-slice2-onwards.md` |
| SubAgent 活动行 | [tui-subagent-activity.md](tui-subagent-activity.md) | TUI redesign active issue |

## 维护要求

1. 新设计先确定状态、scope、代码事实源和对应 active issue。
2. 实施完成后删掉 issue 进度叙事，只把稳定结果同步为“现行设计”。
3. 文件改名、合并或删除时，同步根/模块路由、standards、code-index 与 active spec
   引用，并运行本地链接检查。
4. 动态 inventory、固定源码行号和命令输出不复制进设计；定位信息放
   `docs/code-index/`。
