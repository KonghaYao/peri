# 阶段一：从工具行为走向任务有效性

本次核查基点 `85f0f612`。可运行入口已有数据体检、行为统计、抽样、证据回查和窗口比较；没有任务质量、任务级完成度、任务分组或提示词效果归因。`src/reporting/report.ts` 将 completion 列为 unavailable，`src/reporting/quality.ts` 对满意度也要求显式人工证据。

历史 `long-session-study.md` 与 `guide-peri-improvements.md` 做过人工分类、完成度推断和用户纠正研究，但没有可持续的评审标签契约；本阶段不继承其中的数字。

试点采用 [任务有效性方法契约](../../TASK-EVALUATION.md)：会话取样、焦点任务评价；把交付、验证、约束、反馈和策略分开，再派生可解释分组。两个 subagent 独立评审，保留边界分歧和未知；协调者回查，而不是投票制造确定性。

只读勘察发现，`2026-08-14T00:00:00Z` 至 `2026-09-14T00:00:00Z` 创建窗口在本次读取时有 851 个可见主会话，其中 289 个缓存 message_count 为零。该窗口包含尚未结束的读取日，数字是元数据勘察；正式抽样改用截至 `2026-09-13T00:00:00Z` 的完整 30 天，并重新计算实际自有记录数。空记录会话列为无法评价，不能计入失败。勘察产物在本地忽略目录 `output/task-effectiveness/probe/`，源数据仍可能继续变化。

接下来的阶段成果：

1. 实现只读、可复现的任务事实包与评审校验/汇总入口；用合成数据验证截断、未知、引用和分歧。
2. 对固定真实样本运行双评审，给出任务分组、评审分歧、代表性证据和可供用户校验的案例。
3. 交付专用方法论 skill，以及由案例支持的提示词方向和可执行实验卡；明确观察结论与尚未验证的改善假设。

验收看实际引用和计算产物：不能用工具成功率代替任务交付，不能将日志结束当失败，不能把用户沉默当验收，不能把两个模型一致当准确率。当前生产 prompt 的入口是 `peri-acp/src/session/frozen.rs::SessionManager::build_frozen_data`、`peri-acp/src/prompt/mod.rs::PromptTemplate`、`peri-acp/prompts/sections/` 及 middleware 的 prompt contribution；历史关联不足时只定位改动候选，不宣称根因已证实。
