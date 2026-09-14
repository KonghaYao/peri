# Peri 研究候选修复与验收

来源是 `../2026-09-13-peri-improvements/` 的五项观察候选，研究基线为 `83435d20`。用户随后授权全部记录 issue、修复、验证及阶段提交；五份 issue 已在 `73df17f7` 保存原始证据、边界和验收场景。关闭后按仓库文档标准移除 active issue，Git 保留问题历史，本目录保存阶段成果。

历史观察用于决定检查方向，不能证明机制或改进效果。实现以当前生产代码和可复现用例为准；Luna 承担模块实现与独立审查，协调者编写方法、复核证据并提交。

## 已完成修复

| 候选 | 当前状态 | 可校验成果 |
| --- | --- | --- |
| `WORKFLOW-GRAMMAR` | 已提交 `0c5aa9a3` | [语法契约、生成试验与限制](workflow-eval/README.md) |
| `CORRECTION-SCENARIO` | 已提交 `68da6dd5` | [提示词与 skill、盲评和裁决](correction-eval/README.md) |
| `VERIFICATION-EVIDENCE` | 已提交 `4d233f43`、`503ed45c`；验收 `a5919faf` | [结构化执行事实、真实命令 capture 与旧库兼容](execution-evidence/README.md) |
| `AGENT-SUGGESTIONS` | 已提交 `117e9dd3`；提示词补漏 `9495928b` | [双 cwd、真实 loader、动态定义与 MCP 边界](agent-suggestions/README.md) |
| `LLM-ERROR-CONTEXT` | 已提交 `d370c4bf` | [安全诊断、真实 Runtime fixture、持久化往返与 ACP 验收](model-error-context/README.md) |

五项候选均已完成工程验收并关闭 active issue。最终隔离快照通过 3,676 项库测试与 8 项 doc tests；7 项既有 ignored 用例未计入通过。全目标 clippy 与正常提交 hooks 通过。

表中候选 ID 的完整前缀均为 `PERI-20260913-`。这里只记录本次五项修复；同一工作区其他任务的 ADLC 改动不属于本报告。

## 阶段研究结论

Workflow 的合法示例通过真实 Node parser；额外 export、import、缺 meta、旧 API 和非法 cwd 仍被相应边界拒绝。四个固定生成任务里，基线首次缺 meta，候选首次生成的文件存在 JSON 编码问题；两侧都需要一次反馈修复。因此不能只统计 parser 错误就宣称总修复轮次下降。语法预检也无法证明 Workflow 返回了有意义的目标结果。

纠正场景实验覆盖 8 个唯一任务，每版两次，共 32 份下一步决策。两版在目标、证据、行动与授权维度均通过；澄清维度各有一项证据不足的分歧。结果未证明候选优于基线，也没有验证真实生产执行。原评审、揭盲前裁决和版本 hash 均保留。

旧库只读兼容检查覆盖全项目、全日期的 10,909 个会话与 564,115 条消息，不是前一轮 48 个 Peri root 的研究总体。JSON 解析全部成功，但有 24 项归一化告警、4 个孤立结果和 1 个未配对调用；不能把命令退出 0 表述为全部质量检查通过。

## 方法继承

[auto-data-researcher](../../../../.claude/skills/auto-data-researcher/SKILL.md) 增加结构化执行事实的来源、一致性与未知状态判断；[agent-task-evaluator](../../../../.claude/skills/agent-task-evaluator/SKILL.md) 的[实验卡](../../../../.claude/skills/agent-task-evaluator/references/experiment-card.md)进一步区分决策、编码、预检、执行和目标验收，要求冻结版本、保留全部修复轮及盲评分歧。方法由协调者编写，已在 `7bf96b18` 提交并同步至本机启用副本；`4231b803` 继续补充生产者观测契约变化的比较规则，skill 校验和启用副本一致性检查通过。格式/单元验证、合成行为试验和真实任务效果分别报告。

命令退出成功不等于目标测试实际运行；后台启动不等于工作完成；持久化输出引用存在不等于现在可读。上述区分是后续任务评价的证据边界，不是替历史任务补写通过标签。

`9495928b` 继续补充真实生产字段写入后重读的兼容性检查，避免把 JSON 写出成功或手造 DTO 的自洽测试当作持久化契约已成立。skill 格式校验通过，启用副本与仓库事实源一致；这不是生产任务行为效果的证明。
