# Workflow agent 报告代码变更但实际未写盘（6/12 agent 输出幻觉）

**状态**：Open
**优先级**：高
**类型**：Bug — Workflow 系统
**创建日期**：2026-07-22
**来源**：`issue-resolution-devflow` workflow 执行复盘
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

workflow 链路已随 3.0 重构（SessionFactory/exec 拆分）重构；但没有任何可静态证明「报告写盘一致性」的契约断言（journal 声称 vs git diff 校验），幻觉误报风险无法静态排除——**待运行时复测**。

**状态**：Open（保持）

## 问题描述

在 `issue-resolution-devflow` workflow 中，12 个 agent 有 6 个在 journal 输出中详细描述了代码变更（含文件路径、行数、编译结果），但实际检查发现**没有任何文件被写入**。Workflow 状态为 `completed`，`cargo build` 通过，但 git diff 中不包含这 6 个 agent 声称修改的任何文件。

## 症状

| Agent | Journal 声称 | 实际 git diff | tokenCount |
|-------|:-----------:|:------------:|:----------:|
| P0 (LLM error) | 修改 4 文件，报告编译通过 | **0 文件变更** | 21,930 |
| P1-3 (error cleanup) | 新增 2 错误变体，修改多文件 | **0 文件变更** | 7,736 |
| P1-4 (cancel token) | 修改 retry.rs | **0 文件变更** | 5,223 |
| P1-5 (CI gates) | 修改 lefthook.yml + 创建 CI config | **0 文件变更** | 1,431 |
| P1-6 (API stability) | 标记 18 类型 + 9 模块 doc(hidden) | **0 文件变更** | 16,078 |
| P2-1 (agm unwrap) | 修改 filter.rs | **0 文件变更** | 677 |

对比成功写入的 agent（P1-1/P2-2/P2-3/P2-4），这些 agent 的 journal 输出同样详细且格式规范，无法从输出文本区分真假。

## 可能根因

### 假设 A：Agent 输出幻觉（描述计划但未执行）
Agent 在收到"按 devflow 流程"的指令后，可能将 explore+plan 的输出当作了最终交付物，在文本中"模拟"了代码变更的过程，但从未实际调用 Write/Edit 工具。

**证据**：
- 6 个失败 agent 的 toolCount 均为 0（journal 中无 tool call 记录）
- 成功 agent 大量使用了 Read/Write/Edit/Bash 工具

### 假设 B：Workflow 沙箱写盘静默失败
某些 Write/Edit 操作在 workflow 沙箱中静默失败，agent 未感知到写入失败，继续报告"编译通过"。

### 假设 C：多 Agent 并发冲突导致回退
6 个 agent 在同一 Phase 并行执行时，某些 agent 的写入被其他 agent 的写入覆盖或回退。

## 复现条件

- **复现频率**：6/6 同类 agent 全部失败
- **触发条件**：agent prompt 中包含"按 devflow 流程处理"的描述性指令，agent 可能倾向于输出文本报告代替实际执行

## 涉及文件

（无代码变更，纯 workflow 系统问题）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
