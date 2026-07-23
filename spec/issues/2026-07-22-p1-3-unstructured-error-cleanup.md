# P1-3：非结构化错误清理

**状态**：Open
**优先级**：中
**类型**：代码质量
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 错误处理维度

## Problem Statement

多处错误处理绕过了项目规范（库 crate 用 thiserror，错误消息用英文），使用非结构化类型：

| 位置 | 问题 | 影响 |
|------|------|------|
| `peri-agent/src/error.rs:33-34` | `AgentError::Other(#[from] anyhow::Error)` — anyhow 自动转换绕过所有结构化变体 | 调用方无法 match 具体错误做分级处理 |
| `peri-middlewares/src/subagent/background.rs:187` | `register_with_kind` 返回 `Result<(), String>` — 用裸字符串代替结构化错误 | 上游无法判断注册失败的具体原因 |

## 建议方案

1. **AgentError::Other**：审计 `AgentError::Other` 的所有构造点，将高频错误提升为独立变体（如 `AgentError::ContextOverflow`、`AgentError::ToolNotFound`）。保留 `Other` 仅用于真正不可预见的错误。

2. **register_with_kind**：定义 `enum SubagentRegistrationError { AlreadyRegistered, MaxConcurrencyReached, ... }`，替换 `String` 返回值。

## 涉及文件

- `peri-agent/src/error.rs:33-34`
- `peri-middlewares/src/subagent/background.rs:187`
- 所有构造 `AgentError::Other(...)` 的调用点

## 风险

- **低**：仅增加错误变体，向下兼容。`AgentError::Other` 保留不删除，不影响现有匹配
