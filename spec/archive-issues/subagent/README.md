# subagent

SubAgent——Background/Fork Agent、工具卡片、事件转发、loading

| # | 日期 | 标题 | 状态 |
|---|------|------|------|
| 1 | 2026-07-05 | [SubAgent 展开区工具调用全部显示且有空行，应截断到后 5 个并移除区内空行](2026-07-05-subagent-toolcard-truncate-and-nospacing.md) | Fixed |
| 2 | 2026-07-07 | [bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区](2026-07-07-bg-agent-complete-no-resume.md) | Fixed |
| 3 | 2026-07-07 | [Fork 模式 SubAgent 收不到父对话历史](2026-07-07-fork-subagent-no-parent-conversation-history.md) | fixed |
| 4 | 2026-07-07 | [SubAgent 卡片完全不显示（SubagentStarted 事件被 notifier 丢弃）](2026-07-07-subagent-group-header-shows-agent-instead-of-task-description.md) | Fixed |
| 5 | 2026-07-09 | [Agent 工具（background 模式）启动 bg agent 后 loading 不停止](2026-07-09-bg-agent-loading-never-stops-after-first-turn.md) | fixed |
| 6 | 2026-07-10 | [后台 subagent 完成通知中的"工具调用"计数始终为 0，与 agent 实际调用的工具数不一致](2026-07-10-bg-subagent-tool-count-always-zero.md) | Fixed |
| 7 | 2026-07-10 | [SubAgent 内部工具调用完成后 ⎿ 详情行不应显示](2026-07-10-subagent-toolcard-detail-lines-shown-after-done.md) | Fixed |
| 8 | 2026-07-11 | [多 bg agent 全部回调正常但 loading 仍不退出](2026-07-11-bg-multi-agent-loading-callback-ok-but-loading-stuck.md) | Fixed |
| 9 | 2026-07-11 | [单轮内多次 bg agent 完成后 loading 卡死 + 最后一个 callback 消息丢失](2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md) | Fixed |
| 10 | 2026-07-11 | [hung bg agent 导致 run_react_loop await_wake 永久阻塞](2026-07-11-hung-bg-agent-await-wake-block-forever.md) | Fixed |
| 11 | 2026-07-13 | [同步 Agent 子工具调用卡片完全不显示](2026-07-13-sync-agent-tool-cards-not-showing.md) | Fixed |
| 12 | 2026-07-16 | [子 agent 工具别名解析失败](2026-07-16-subagent-tool-alias-not-resolved.md) | Fixed |
| 13 | 2026-07-18 | [清理 SubagentStarted 的 EventBus 残留路径（死代码 + 双重发送陷阱）](2026-07-18-cleanup-subagent-eventbus-dead-code.md) | Fixed |
| 14 | 2026-07-18 | [SubAgent 工具调用卡片回归不显示（Agent 卡片容器可见但内部为空壳）](2026-07-18-subagent-tool-cards-regression-empty.md) | Fixed |
| 15 | 2026-07-18 | [SubAgent 沙箱写工具（WriteSandbox）：让 planner 等 readonly agent 能输出交接文件](2026-07-18-subagent-write-sandbox-tool.md) | Done |
| 16 | 2026-07-20 | [plan agent 偶发缺少 WriteSandbox 工具——沙箱目录不存在时构造失败导致静默跳过](2026-07-20-plan-agent-writesandbox-not-found.md) | Fixed |
| 17 | 2026-07-20 | [WriteSandbox 工具仍容易被 subagent 误认为 Write——路径不在沙箱白名单内反复报错](2026-07-20-writesandbox-still-confused-with-write.md) | Fixed |
| 18 | 2026-07-21 | [同一 turn 触发两个 Agent 时，子工具未嵌套在对应 Agent 下方](2026-07-21-multi-agent-tools-wrong-grouping.md) | Fixed |

*共 18 条*
