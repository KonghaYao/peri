# ADR：Langfuse 监控 v2 架构重设计

> 日期：2026-07-14 | 决策者：KonghaYao + AI

## Context

当前 langfuse 监控覆盖弱（仅 9 个 ExecutorEvent 被转发，ReAct 5 阶段、14+5 中间件链、
ContextBudget 阈值点、Compact 三级策略、MessageQueue、Workflow、AiReasoning 等核心架构
盲区），trace_id 与 turn_id 脱节（违反架构文档 §2.6「turn_id 作为统一纽带」），无 Sampling
机制，LangfuseTracer 内部 14 字段散在 6 个 handler 文件中。

## Decision

方案 B 一次性大重构：
1. **三层映射**：1 Session → N Trace (trace_id=turn_id) → 5 Stage Span
2. **LangfuseTracer 收敛**：14 字段 → 5 简单字段 + 7 子状态机
3. **12 个新 ExecutorEvent 变体 + 2 个扩充**
4. **Turn 级 Sampling**（hash + rate）+ **ErrorSpan 兜底**
5. **配置**：5 环境变量全部支持 settings.json

## Alternatives

- 方案 A 分阶段：被否决（残留中间状态、用户明确要求激进）
- 方案 C 最小补丁：被否决（盲区仍在）
- 预判机制：被否决（用户拒绝，简化为纯 hash + rate）

## Consequences

- 单大 PR 8 commit，每个 commit 独立可编译可测
- 旧 trace schema 与新 schema 在 Langfuse 后端共存
- 后续改动收敛在 7 个子对象层
- 测试：321 单测 + 2 e2e mock + variant_coverage_test

## Compliance

- `cargo test --workspace` 全 PASS
- 14 个 P0 测试矩阵全过
- variant_coverage_test 覆盖所有新变体
