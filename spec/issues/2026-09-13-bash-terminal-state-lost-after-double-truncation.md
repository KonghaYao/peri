# Bash 长输出经过双层截断后丢失终态证据

**状态**：Open  
**优先级**：高  
**类型**：缺陷 / 运行时证据 / 输出契约  
**创建日期**：2026-09-13  
**来源**：本轮用户授权记录；`side-projects/agent-defect-analyzer/reports/2026-09-13-peri-improvements/improvement-queue.json` 中 `PERI-20260913-VERIFICATION-EVIDENCE`

## 问题描述

当 Bash 命令产生较长输出时，终端层先按行数或字节数截断并把完整内容写入临时文件，Agent tool dispatch 随后又按工具的 `output_char_limit` 取正文前缀。最终返回给模型、持久化消息和缺陷分析器的正文可能不含退出标记或完整输出文件引用。这样，在长测试或构建结束后，调用者可能只有一段被截断的文本，却无法从稳定字段判断命令是否成功、失败、取消或仍在后台运行。

## 症状详情

- 观测队列记录了 6 个抽样会话中的 2 个 medium focal task 存在验证证据不足。这是审计样本中的计数，不是任务失败率或总体发生率。
- 研究证据定位：thread `01a07b52-aa80-7ef2-9048-4eda8106c5b4` 的 messages `01a07b54-ac03-7eb1-aa79-151b9cfd8e21`、`01a07b54-f654-7d72-a185-fa6709d5f50e`；thread `01a083e3-5c9e-7df0-89fa-7a0b43ee4eba` 的 messages `01a083e7-98ed-7351-ac00-47881b4e2c1c`、`01a083e8-5041-79f0-8767-298ae6cd01cf`。
- `peri-middlewares/src/middleware/terminal.rs:58-100` 的 `truncate_output` 在输出超过 2,000 行或 65,000 字节时持久化后截断，并把提示拼到正文；`merge_output` 在 `:106-129` 生成非零退出标记。
- `peri-agent/src/agent/stages/tool_dispatch/execution.rs:431-466` 的 `post_process_result` 在建议文本注入后按工具 `output_char_limit` 再截断；Bash 在 `peri-middlewares/src/middleware/terminal.rs:531-532` 声明的上限为 10,000 字符。
- `side-projects/agent-defect-analyzer/src/data/loader.ts:135-238` 与 `src/research/task-reviews.ts:64-122` 能识别 message/export 的截断标志，但当前证据链没有为每个 Bash 执行提供独立、稳定的退出状态字段。

## 现行观察与待验证假设

### 现行观察

- 生产路径中确实存在终端输出处理和 tool dispatch 两个长度边界，且它们作用于同一个可见正文。
- 终端层的完整输出保存提示位于截断正文之后；dispatch 层只保留结果前缀再追加自己的截断提示。
- 当前源代码审计尚未运行四类生产全链路 fixture，也没有证明每一次历史正文截断都实际删除了退出标记。

### 待验证假设

- 当退出标记、取消状态或保存路径位于正文后部时，第二层截断可能使分析器和模型无法恢复对应终态。
- 结果正文与持久化消息之间的投影可能进一步放大“全部通过”的自报，或把未知状态误读为失败；这需要生产导出和分析器复核才能确认。

这里不把两层截断与历史审计中的误读相关性表述为根因。

## 验收场景

1. 通过生产 Bash tool pipeline 和持久化导出运行四个 fixture：大输出末尾失败、后台任务未终止、成功、取消；每个 fixture 都能按稳定执行身份恢复终态，且退出码/取消/后台状态不依赖正文尾部。
2. 正文保持有界，完整输出沿用现有保存与清理机制；正文被截断时，完整输出引用和截断事实可被下游读取，不能把未知算作通过。
3. 已完整通过的检查不会因投影缺少尾部而被误判失败；失败、取消和未终止不会被“全部通过”覆盖。
4. analyzer review 对直接运行证据、自报文本和目标环境未知保持区分；现有输出上限、脱敏和后台任务语义不回归。

## 范围边界

- 范围限于 Bash/Terminal 结果、Agent tool-result 投影、持久化导出和缺陷分析消费链路；是否修改共享 ToolResult 契约需由修复阶段依据 fixture 决定。
- 不以在 Bash 文本末尾追加提示作为充分修复；也不在本 issue 中泛化到所有工具的输出限制，除非共享契约证据显示同一问题。
- 不据此断言历史命令失败、模型当时看到了完整输入，或某个历史任务的根因已经确定。

## 复现条件

- **复现频率**：待生产 fixture 验证；当前仅有抽样观察。
- **触发步骤**：
  1. 执行输出超过终端行/字节阈值的 Bash 命令，并让成功或失败状态出现在输出尾部。
  2. 让结果经过 Agent dispatch、消息持久化和 analyzer 导出。
  3. 对照原始执行状态、可见正文、保存引用和分析结论。
- **环境**：Peri 生产 Bash/Agent 路径；具体模型、平台和持久化导出格式待 fixture 固定。

## 涉及文件

- `peri-middlewares/src/middleware/terminal.rs` —— 合并、截断、落盘和 Bash 输出上限。
- `peri-agent/src/agent/stages/tool_dispatch/execution.rs` —— tool-result 建议注入与第二层正文截断。
- `side-projects/agent-defect-analyzer/src/data/loader.ts` —— 持久化消息及截断元数据加载。
- `side-projects/agent-defect-analyzer/src/research/task-reviews.ts` —— 截断包的审查约束。
- `peri-acp/prompts/sections/03_doing_tasks.md` —— 完成主张与运行证据的用户侧提示边界。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-13 | — | Open | agent | 依据本轮用户授权和观察队列创建；待实际执行验证 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
