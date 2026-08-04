# 全局索引

Peri 项目全局领域知识索引。

## 领域索引

| 领域 | 描述 | 文件 |
|------|------|------|
| agent | Agent / ReAct 循环、工具系统、Context 管理、SubAgent 构建、LLM Provider 桥接 | [domains/agent.md](domains/agent.md) |
| tui | TUI 前端渲染、交互、面板、输入处理、状态管理 | [domains/tui/tui-index.md](domains/tui/tui-index.md) |
| model | peri-model：provider 无关协议 DTO、流式优先 Model trait、OpenAI / Anthropic 适配、HTTP/SSE 传输与重试观测 | [domains/model.md](domains/model.md) |
| acp | ACP 服务层：session 生命周期、prompt 构建、中间件装配、事件映射/发送、Langfuse 观测、transport 传输 | [domains/acp.md](domains/acp.md) |
| middlewares | 中间件与工具生态：MCP / Plugin / Skills 接入、SubAgent 构建与调度、HITL 审批、Workflow 编排、Hook / Goal / Cron 横切能力 | [domains/middlewares.md](domains/middlewares.md) |

## 问题索引

按关键词索引已归档 issue，见 [problems.md](problems.md)。

## 全局约束

（后续通过 sdd-global-init 或 issue 归档逐步填充）
