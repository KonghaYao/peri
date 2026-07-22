# 合并 AcpAgentConfig 和 PromptExecutionContext 为 SessionContext

**状态**：Fixed
**优先级**：高
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #1（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp` 中存在两个高度重叠的大型参数对象：

- **`AcpAgentConfig`**（`agent/builder.rs`）：34 个字段，用于 `build_agent()` 和 `build_stage_context()`
- **`PromptExecutionContext`**（`session/executor.rs`）：33 个字段，在 `run_session_loop` 中构造后传递给 executor 函数

两个 struct 共享 20+ 个相同字段（provider、peri_config、cwd、session_id、cancel、broker、permission_mode、plugin_config、hook_groups、cron_scheduler、mcp_pool、channel_state、shared_tools、tool_search_index、thread_store、thread_id、lsp_servers 等）。

**影响**：
- 新增字段需要同时修改两个 struct 定义 + 两个构造点 + `build_agent` 解构
- 一个字段漏加或不一致 = 运行时错误
- 两个 struct 之间的映射关系隐式约定，无编译期保证
- `PromptExecutionContext` 在 executor 中构造后，大部分字段仅用于透传给 builder

## 建议方案

将两者合并为单一 `SessionContext` struct，按子模块做命名空间分组：

```rust
pub struct SessionContext {
    pub config: PeriConfig,
    pub session: SessionHandle,     // session_id, thread_id, cancel, message_queue
    pub infra: InfraServices,       // broker, cron_scheduler, mcp_pool, channel_state
    pub tools: ToolRegistry,        // shared_tools, tool_search_index, lsp_servers
    pub frozen: FrozenScope,        // frozen_data, thread_persistence
    pub permissions: PermissionMode,// permission_mode, yolo
}
```

在 `run_session_loop` 入口构造一次，后续全部函数接收 `&SessionContext`。

## 涉及文件

| 文件 | 改动 |
|------|------|
| `agent/builder.rs:1019` | 删除 `AcpAgentConfig`，改为接收 `&SessionContext` |
| `session/executor.rs:1241` | 删除 `PromptExecutionContext`，改为构造 `SessionContext` |
| `session/executor_helpers.rs:801` | `build_and_execute_agent_v2` 签名改为 `&SessionContext` |
| `agent/workflow_agent.rs:666` | Workflow agent 构建路径适配 |

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`5f42746d refactor(peri-acp): 合并 PromptExecutionContext 到 SessionContext`
- **修复内容**：合并 AcpAgentConfig + PromptExecutionContext 为单一 SessionContext

## 风险

- 改动涉及多个核心函数签名，需要全面测试覆盖
- `FrozenData` 和 `ThreadPersistence` 的分组需要与当前 `FrozenSessionData::build()` 匹配
- v1（`build_agent`）和 v2（`build_stage_context`）两条链路需同时适配

## 关联 Issue

- 消除 executor 三层参数透传（`2026-07-20-acp-flatten-executor-pipeline.md`）——本 issue 的改动自然推动该 issue 的完成
- 删除退化模块（`2026-07-20-acp-remove-degenerate-modules.md`）——合并后 `frozen.rs` 可内联
