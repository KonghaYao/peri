# 消除 executor 管线三层参数透传

**状态**：Fixed
**优先级**：高
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #2（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp` 的 executor 管线存在三层参数透传反模式：

```
run_session_loop(PromptExecutionContext: 33 字段)
  → build_and_execute_agent(InterceptRequest: ~17 字段)
    → build_and_execute_agent_v2(17 个位置参数)
      → build_agent(AcpAgentConfig: 34 字段)
```

每层函数都从上一层的参数对象中解构部分字段、透传部分字段。特点：
- 中间层（`build_and_execute_agent` / `build_and_execute_agent_v2`）不消费大部分参数，只做搬运
- 添加新字段需要在所有三层签名的正确位置插入
- 跟踪某个字段的完整流动路径需要阅读三个函数
- `build_and_execute_agent_v2` 的 17 个位置参数已经触发了 clippy 警告并加了 `#[allow]` 注解

## 建议方案

在合并 `AcpAgentConfig` + `PromptExecutionContext` 为 `SessionContext` 后，将所有 executor 管线函数改为接收 `&SessionContext` 或 `Arc<SessionContext>`：

```rust
// Before
async fn build_and_execute_agent(
    provider: LlmProvider,
    peri_config: Arc<PeriConfig>,
    session_id: SessionId,
    // ... 14 个更多参数
)

// After
async fn build_and_execute_agent(ctx: &SessionContext)
```

移除不再需要的中间层 `InterceptRequest` struct（如果所有字段都已在 `SessionContext` 中）。

## 涉及文件

| 文件 | 改动 |
|------|------|
| `session/executor.rs` | `run_session_loop` 改为接收 `SessionContext`；`build_and_execute_agent` 合并参数 |
| `session/executor_helpers.rs` | `build_and_execute_agent_v2` 改为接收 `SessionContext`；评估是否可合并回 executor.rs |
| `agent/builder.rs` | `build_agent` / `build_stage_context` 改为接收 `&SessionContext` |

## 收益

- **locality**：字段流动路径从 3 层 → 1 层（所有函数直接从 `SessionContext` 读取）
- **leverage**：新增字段只需在 `SessionContext` 中加一项，所有调用方自动可见
- 删除 `#[allow(clippy::too_many_arguments)]` 注解
- executor 函数可独立测试（只需构造 `SessionContext` 而非 17 个位置参数）

## 前置依赖

- 合并 AcpAgentConfig + PromptExecutionContext（`2026-07-20-acp-merge-config-params.md`）

## 风险

- 如果 `SessionContext` 某个字段需要在管线中途被修改（而非只读），需要改用 `Arc<RwLock<SessionContext>>` 或拆分可变部分
- v1/v2 双轨需要在两条路径上同时验证

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`c5431160 refactor(peri-acp): 删除 AcpAgentConfig，消除 executor 管线参数透传`
- **修复内容**：删除 AcpAgentConfig struct，合并参数透传链，简化 executor 管线
