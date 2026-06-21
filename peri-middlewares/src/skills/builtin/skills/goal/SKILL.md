---
name: goal
description: >
  长程目标跟踪。当用户给出需要多步骤完成的复杂任务时使用。
  触发词："goal"、"目标"、"持续执行直到完成"、"直到 X 为止"。
userInvocable: true
argumentHint: "[objective description]"
---

# Goal 模式

## 何时使用

- 用户给出复杂任务，需要多轮执行才能完成
- 用户说"持续执行直到完成"、"不要中途停下"、"直到 X 为止"

## 如何使用

1. 调用 `goal` 工具，action=create，objective 设为具体可验证的目标描述
2. 持续工作，直到目标达成
3. 达成 → `goal` 工具，action=complete
4. 遇到无法解决的阻塞 → `goal` 工具，action=block，reason 填写阻塞原因
5. 随时可用 action=get 查询当前目标状态

## 重要约束

- **目标必须具体可验证**："优化代码"不好，"将测试覆盖率提到 80%"好
- **complete 会经验证**：系统会用辅助 LLM 判断你是否真的达成了目标，未通过会返回原因
- **block 是求救信号**：只在真正无法继续时使用（如缺权限、缺依赖）
- **创建后自驱**：goal 创建后，每轮结束时会收到提醒，你必须决策：继续/完成/阻塞
- **单例**：同一时间只能有一个 goal，创建新 goal 前需先 clear
