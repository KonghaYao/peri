# agent-core

Agent 引擎——Compact 策略、EventBus、Goal 循环、executor 管线、SystemNote

| # | 日期 | 标题 | 状态 |
|---|------|------|------|
| 1 | 2026-07-07 | [后台任务统一管理（bg agent / workflow / bg shell）](2026-07-07-bg-tasks-unified-management.md) | Fixed |
| 2 | 2026-07-08 | [peri-agent 架构设计改进](2026-07-08-peri-agent-architecture-improvement.md) | Fixed |
| 3 | 2026-07-12 | [Agent 工具调用内部的子工具调用跑到历史消息区，未嵌套在 Agent 卡片下方](2026-07-12-agent-nested-toolcall-misplaced-into-history.md) | Fixed |
| 4 | 2026-07-13 | [工具调用统一 300s 超时导致 Agent/SubAgent 正常任务被强制中断](2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks.md) | Fixed |
| 5 | 2026-07-15 | [Goal 自驱续跑在 v2 架构下完全断裂](2026-07-15-goal-continuation-loop-broken-in-v2.md) | Done |
| 6 | 2026-07-16 | [统一事件发射路径：所有 Agent 事件走 v2 EventBus](2026-07-16-eventbus-unified-emission.md) | Done（方案 B 已完成 CompactStrategy 硬编码 + Path D 代码组织改善全部实施；方案 A 主体 NOT READY，4 项经两轮对抗评审判定搁置） |
| 7 | 2026-07-16 | [Cache 命中率警告 SystemNote 在消息流中位置错位——被积压到上一个 user/AI message 后面](2026-07-16-system-note-cache-warning-position-wrong.md) | Fixed |
| 8 | 2026-07-17 | [Compact 标记（truncated/excluded）在 Session 恢复后丢失，导致上下文直接到 100%](2026-07-17-compact-flags-lost-on-session-restore.md) | Fixed |
| 9 | 2026-07-18 | [compact 目录物理删除——config.rs 上移到 compact_v2 并消除空壳](2026-07-18-compact-directory-removal.md) | Done |
| 10 | 2026-07-18 | [Compact 效果在 v2 路径中跨 prompt 丢失，上下文使用率每轮重置到 100%](2026-07-18-compact-effect-lost-between-prompts-v2.md) | Fixed |
| 11 | 2026-07-20 | [消除 executor 管线三层参数透传](2026-07-20-acp-flatten-executor-pipeline.md) | Fixed |
| 12 | 2026-07-20 | [合并 AcpAgentConfig 和 PromptExecutionContext 为 SessionContext](2026-07-20-acp-merge-config-params.md) | Fixed |
| 13 | 2026-07-20 | [Cache 命中率警告 SystemNote 位置错位——多警告时全部出现在 user 消息正下方](2026-07-20-cache-warning-systemnote-position-regression.md) | Fixed |
| 14 | 2026-07-21 | [删除 YOLO_MODE 环境变量，全部改用 PermissionMode 系统](2026-07-21-remove-yolo-mode-env-var.md) | 已修复 |
| 15 | 2026-07-22 | [Full Compact 摘要 Prompt 严重落后于 Claude Code，影响压缩后 Agent 恢复质量](2026-07-22-compact-summary-prompt-behind-claude-code.md) | Fixed |
| 16 | 2026-07-22 | [Micro/Full Compact TUI 通知标签互换 bug](2026-07-22-compact-tui-label-micro-full-swapped.md) | Fixed |
| 17 | 2026-07-22 | [Micro Compact 触发策略设计落后于 Claude Code，压缩效果不足](2026-07-22-micro-compact-trigger-strategy-behind-claude-code.md) | Fixed |

*共 17 条*
