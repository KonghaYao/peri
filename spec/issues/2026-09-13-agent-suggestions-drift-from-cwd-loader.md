# Agent 建议与实际 cwd/loader 来源漂移

**状态**：Open  
**优先级**：高  
**类型**：缺陷 / 工具诊断 / 生命周期一致性  
**创建日期**：2026-09-13  
**来源**：本轮用户授权记录；观察队列 `PERI-20260913-AGENT-SUGGESTIONS`

## 问题描述

Agent 定义加载失败后，错误建议可能从会话初始化时的 registry snapshot 产生，而真正的 Agent 调用会按调用 cwd、冻结的 built-in policy、plugin 目录和 MCP registry 再解析。两套来源可能列出相同名称，却不能保证该名称能在本次调用中加载。用户按建议重试时仍可能得到同名失败，恢复提示因此没有把下一次调用变成有效调用。

## 症状详情

- 观察队列记录 2 个显式错误、1 个受影响 root thread；分母是同一 48 roots 样本中的 24 个 Agent 显式错误，不能解释为普遍失败率。
- 研究证据定位：thread `01a08429-fe60-7b72-8f2a-6dcf13d6728d` 的 messages `01a0895a-53cb-7c30-b418-a3dee35d618b`、`01a08984-490e-7b21-9135-d66209bc34d6`；对应 calls `call_ZFn3auUcsOyqnELwoSuLpaiw`、`call_bVIidUuyEPqMW69sRfUUvDGv`。
- `peri-middlewares/src/error_suggest/suggesters/subagent_suggester.rs:22-65` 只从 `ErrorContext.tool_registry.subagent_types` 排序和 fuzzy match 后生成建议。
- `peri-middlewares/src/error_suggest/default_registry.rs:26-75` 的 snapshot 构造包含 built-in 类型和传入的 `.claude/agents` 目录扫描，但没有体现每次调用 cwd、plugin agent dirs、MCP activation 或冻结 policy。
- `peri-middlewares/src/subagent/tool/definitions.rs:10-97` 的真实 loader 按 cwd 查 project candidates，再按是否启用 built-ins、plugin dirs 和 MCP registry 分支解析。
- `peri-agent/src/agent/stages/tool_dispatch/execution.rs:440-454` 将 snapshot 放入错误建议上下文；它改变的是可见 output 文本，不改变实际 loader。

## 现行观察与待验证假设

### 现行观察

- snapshot 构造与真实 `load_agent_def_with_built_ins` 读取的输入集合、cwd 和 policy 参数并非同一接口。
- source audit 能确认这些来源不同，但历史数据不能重建定义是否在会话中途被删除，也不能单凭错误文本证明某个建议当时不可加载。
- 同一名称在调用 cwd 中真实可加载时，仍应保留 typo/fuzzy suggestion 能力。

### 待验证假设

- 当 snapshot 含有 A cwd 的定义、调用实际发生在没有该定义的 B cwd 时，建议可能显示不可加载的同名项。
- plugin-only、MCP-only 或 built-in policy 被关闭的场景可能产生同类来源漂移。

这些观察只说明两套解析上下文不同，不把历史重试失败直接归因为 snapshot。

## 验收场景

1. 建立临时 A/B 两个 cwd，仅 A 有同名项目定义；在 B 调用同名 Agent，建议不得自荐无法在 B 的实际 loader/policy 上加载的名称。
2. 覆盖 `.claude/agents` 扁平与嵌套格式、`agents/`、plugin-only、built-in enabled/disabled、定义被移除和合法 typo；现有 MCP activation 行为不回归。
3. 每个 fuzzy 候选都能在该调用的真实 cwd、loader 来源和 policy 下重新解析；同名缺失不再生成无效重试建议。
4. 合法发现能力不退化：可加载的近似名称仍给出稳定、有限且脱敏的候选；建议层不绕过现有权限和 policy。

## 范围边界

- 范围限于 Agent error suggestion 的候选来源与真实定义 loader 之间的契约、调用 cwd/policy 传递和相关测试。
- 不在本 issue 中改变 built-in、plugin、MCP 的启用策略，不绕过 `load_agent_def`，也不把所有 session snapshot 都改成无界实时扫描。
- 不推断历史定义删除、项目切换或调用 cwd 变化的具体时间；这些只能由新增 fixture 或运行记录确认。

## 复现条件

- **复现频率**：待双 cwd 和来源矩阵 fixture 验证；当前为审计样本观察。
- **触发步骤**：
  1. 在 A cwd 建立可加载 Agent 定义并构造会话 snapshot。
  2. 在 B cwd 或不同 loader policy 下调用同名/近似 `subagent_type`。
  3. 比较建议候选与真实 `load_agent_def` 结果，再按建议重试。
- **环境**：Peri Agent tool、error_suggest registry、project/plugin/MCP agent sources。

## 涉及文件

- `peri-middlewares/src/error_suggest/suggesters/subagent_suggester.rs` —— fuzzy 建议生成。
- `peri-middlewares/src/error_suggest/default_registry.rs` —— snapshot 与 `.claude/agents` 扫描。
- `peri-middlewares/src/subagent/tool/definitions.rs` —— 实际 Agent 定义加载来源和 policy。
- `peri-agent/src/agent/stages/tool_dispatch/execution.rs` —— 错误上下文装配与建议注入。
- `peri-acp/prompts/sections/11_subagent.md` —— 可用 Agent 目录和调用约束的用户侧说明。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-13 | — | Open | agent | 依据本轮用户授权和观察队列创建；待实际执行验证 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
