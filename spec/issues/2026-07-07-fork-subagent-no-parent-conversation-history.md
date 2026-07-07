# Fork 模式 SubAgent 收不到父对话历史

**状态**：fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

fork 模式的 SubAgent 无法访问父对话的完整历史记录。文档和系统提示词中明确声明 fork 模式"继承完整对话历史"，但实际运行时 fork SubAgent 收到的对话历史为空（或缺失关键内容），导致无法执行需要回顾对话上下文的"对话回顾类"任务。SubAgent 会回复类似"当前对话历史为空，无法提供相关信息"的内容。

## 症状详情

| 维度 | 详情 |
|------|------|
| 什么现象 | fork SubAgent 被调用后，认为对话历史为空，回复"当前对话历史为空" |
| 什么时候出现 | 使用 `Agent(fork: true, prompt: "...")` 派发 fork 子 agent 进行需要回顾父对话历史的任务时 |
| 期望行为 | fork SubAgent 应能看到父 agent 的完整对话消息（Human/AI/工具调用记录等），并基于此上下文执行任务 |
| 实际行为 | fork SubAgent 只能看到 fork directive + prompt 本身的内容，看不到父对话的任何历史消息 |
| 影响范围 | 所有依赖 fork 模式执行"对话回顾类"任务的场景（如：总结已有讨论、基于历史上下文做判断、回顾之前发现的问题等） |

## 复现条件

- **复现频率**：疑似必现（fork 模式下 SubAgent 始终无法获取父对话历史）
- **触发步骤**：
  1. 与主 agent 进行多轮对话，积累一定对话历史
  2. 让主 agent 以 fork 模式派发一个 SubAgent，要求其回顾/总结对话历史中的内容
  3. SubAgent 回复称"对话历史为空"，无法完成回顾任务
- **环境**：v2 stages 架构（`build_v2_subagent_context` + `run_react_loop` 路径）

## 涉及文件

- `peri-middlewares/src/subagent/tool/execute_fork.rs` —— fork 模式的 SubAgent 调用入口，负责将 `parent_messages` 传入 `build_v2_subagent_context`
- `peri-middlewares/src/subagent/v2_bridge.rs:73-79` —— `parent_messages` 注入 transcript 的位置，fork 路径应在此处写入父对话历史
- `peri-middlewares/src/subagent/tool/define.rs` —— SubAgentTool::invoke 中决定传递哪些 parent_messages 给 invoke_fork 的调用点
- `peri-middlewares/src/subagent/fork.rs:60-81` —— fork directive 模板，声明"You have full access to the conversation history above"

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
