# 加强 coder SubAgent 在 system prompt 中的存在感

**状态**：Open
**优先级**：中
**类型**：技术债
**创建日期**：2026-06-17

## 问题描述

Coder 已是 Built-in Agent（`peri-middlewares/src/subagent/built-in/coder.md`，6 工具、200 轮上限、含 Memory Discipline 反循环规则），但 LLM 在实际使用中仍然大量选择 `general-purpose` 而非 `coder` 来执行代码实现任务。

168h 窗口数据：`general-purpose` 174 次 vs `coder` 65 次（差距 2.7x）。在实现类任务中，`general-purpose` 均消息 25，`coder` 均消息 29——虽然均消息略高，但 coder 具备内置的反循环规则（禁止重搜、3 次搜索失败即停止），实际任务完成质量显著更优。

## 症状详情

### 当前 prompt 中 coder 的呈现方式

`peri-tui/prompts/sections/11_subagent.md` 第 20-24 行已包含 Common Agent Patterns：

```markdown
Some tasks follow natural pipelines (e.g. explore→plan→coder→code-review).

- **Implementation pipeline**: `coder` (write code) → `code-reviewer` (review for issues)
```

但 LLM 仍然偏向 `general-purpose`：

| 指标 | general-purpose | coder |
|------|:--------------:|:-----:|
| 168h 使用次数 | 174 | 65 |
| 均消息 | 25 | 29 |
| 实现类任务占比 | 85.4% 纯搜索 | — |

### `{{available_agents}}` 占位符渲染检查

{{available_agents}} 模板通过 `prompt/mod.rs` 中的 `render_agent_list()` 渲染，其输出内容是 agent 定义文件的 YAML frontmatter 第一行 `description`。当前 coder 描述为：

> "Code implementation specialist. Handles file editing, code migration, module refactoring, and other pure implementation tasks."

而 general-purpose 描述为：

> "General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks."

两种描述都是事实性陈述，但没有明确指出 coder 是 **更优选择**（更少上下文消耗、更严格的记忆纪律、内置反循环保护）。

## 期望改进方向

1. **加强 "Implementation pipeline" 引导语**：在 `11_subagent.md` 中明确说明 coder 相比 general-purpose 的优势（反循环保护、更小上下文、更高的编辑产出比）
2. **优化 coder 的 description**：在 frontmatter description 中加入吸引力关键词（如 "preferred for code implementation"、"more efficient than general-purpose"）
3. **考虑在 Common Agent Patterns 中增加反例**：明确什么时候不应该用 general-purpose（如 "Do NOT use general-purpose for implementation work — use coder instead"）

## 涉及文件

- `peri-tui/prompts/sections/11_subagent.md` —— SubAgent 使用引导 prompt
- `peri-middlewares/src/subagent/built-in/coder.md` —— Coder agent 定义（含 frontmatter description）
- `peri-middlewares/src/subagent/built_in_agents.rs` —— Built-in agent 注册
- `peri-acp/src/prompt/mod.rs` —— `{{available_agents}}` 渲染逻辑
- `side-projects/agent-defect-analyzer/docs/report-2026-06-17.md` —— 分析报告（数据来源）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-17 | — | Open | agent | 创建 |

## 修复记录

（待修复后追加）
