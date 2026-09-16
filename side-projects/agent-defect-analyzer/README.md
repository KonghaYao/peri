# Agent Defect Analyzer

Peri 会话数据的只读分析项目。它的目标是把可复核的观测转成 agent 改进候选；统计结果不能直接证明任务失败、用户满意度或修复因果。

分析口径、证据要求和禁止推断见 [ANALYSIS.md](ANALYSIS.md)。数据库 schema 与消息格式以当前 Peri 代码和测试为准，历史报告只作为调查线索，不能代替重新取数。

持续研究方向：[任务关注点漂移与纠正吸收（RQ-FD-001）](FOCUS-DRIFT.md)，跟踪用户纠正、任务返工，以及后续行动与验收是否重新对齐有效目标。

## 当前状态

统一 CLI 的 `inspect` 只读检查数据库结构、消息格式、工具调用配对和数据质量，生成 `quality.json` 与 `quality.md`。历史工具事实包含当前被 compact 排除的消息；子会话自己持久化的消息属于自己的工作，继承快照单独计数。

```bash
cd side-projects/agent-defect-analyzer
bun install --frozen-lockfile
bun run inspect --db ~/.peri/threads/threads.db --out output/quality
bun run report --db ~/.peri/threads/threads.db --out output/behavior
# 指定会话创建窗口，UTC 半开区间
bun run report --db ~/.peri/threads/threads.db --since 2026-09-01T00:00:00Z --until 2026-09-13T00:00:00Z --out output/recent
bun run sample --db ~/.peri/threads/threads.db --out output/sample --seed review-1 --size 5 --scope roots
bun run evidence --db ~/.peri/threads/threads.db --out output/evidence --thread 'THREAD_ID' --message 'MESSAGE_ID'
bun run compare --baseline output/baseline/report.json --candidate output/candidate/report.json --out output/comparison
```

报告默认不含对话正文、工具参数和文件路径。异常证据保留会话与消息 ID，便于在本机复核。`output/` 是被忽略的本地产物目录。缺失能力、未知格式和解析错误必须查看，不能只读总数。

`report` 输出 `report.json` 与 `report.md`，默认分析可见主会话自己持久化的历史。可选 `--scope roots|children|all`、`--include-hidden`；子会话通常隐藏，分析子会话时显式加 `--include-hidden`。时间筛选依据会话创建时间，纳入该会话的全部持久化消息，不能解释为窗口内发生的工具事件。

错误率的分母是已配对且明确记录成功/错误状态的结果。重复调用与连续失败规则输出达到阈值的候选位置数，不能解释为已经确认的缺陷数量。JSON 包含有界的 `threadId/messageId/callId` 证据与 `nextVerification`；按证据复核后再创建修复任务。

`sample` 与 `report` 使用相同的范围过滤器；抽样要求 `--seed` 与 `--size`（1–50），可加 `--min-messages` 限定会话消息数。时间参数必须是带时区的 ISO 时间戳，统一换算为 UTC，并拒绝无效日期或逆序范围。固定 seed、候选集合与算法产生相同样本；样本指纹只覆盖候选和选中会话元数据，不代表全库内容。

`evidence` 按自有消息的 `--thread`、`--message` 定位，`--radius` 默认为 2、范围为 0–20。默认仅输出 metadata；`--include-content` 才输出正文、参数和结果。最终 JSON 最多 64 KiB，截断与省略通过标志和 `omissions` 明示；证据指纹覆盖窗口元数据。继承上下文可在 viewer 中阅读。

工具结果的 `execution` 是独立的持久化事实：`status`、`exit_code`、`output_truncated`、`task_id` 和是否存在 `output_ref` 均按 typed metadata 读取。缺少旧 metadata 的结果保持 `unknown`；正文中的退出码、`is_error=false`、Agent 自报或退出码为 0 都不会被分析器升级为目标任务通过。非法或互相矛盾的 metadata 保留在 `parseIssues`。任务包默认只导出 `hasOutputRef`、`hasTaskId` 等脱敏事实，不导出本地 `output_ref` 或原始 `task_id`；`--include-content` 才允许导出这些引用，分析器也不会自动读取引用文件。正文被包大小限制截断时，execution facts 仍保留。

任务事实包 schema 当前为 2，因为 execution facts 已成为 packet contract 的必需字段；schema 1 的旧 packet 和旧 review 不做旧 hash 兼容，必须重新导出并重新评审。

任务有效性事实包使用实际自有消息数分为短（1–20）、中（21–100）和长（>100），默认每层固定抽取 4 个可见根会话；`--per-stratum` 可设为 1–10。默认只导出消息元数据，加入 `--include-content` 才导出正文、工具参数和结果文本。每个包最多 128 KiB、160 条消息，省略范围和源记录截断会明确记录。

```bash
bun run src/cli.ts task-sample --db ~/.peri/threads/threads.db --out output/task-packets --seed audit-1 \
  --since 2026-08-14T00:00:00Z --until 2026-09-13T00:00:00Z
bun run src/cli.ts task-packet --db ~/.peri/threads/threads.db --out output/task-packet --thread THREAD_ID --include-content
bun run src/cli.ts task-review --packets output/task-packets/task-packets.json --reviews reviews.json --out output/task-review
```

任务评价口径见 [TASK-EVALUATION.md](TASK-EVALUATION.md)，编排方法见
[agent-task-evaluator skill](../../.claude/skills/agent-task-evaluator/SKILL.md)。实际评审需对抽样命令显式加
`--include-content`；metadata 包只能保留未知。review JSON 以 `src/research/task-reviews.ts` 的
`TaskReviewInput` 为准，保留每个 reviewer 的身份、任务边界、包 hash、五维标签和证据 ID。
`task-review` 校验引用和格式，不代替语义复核。`review*` 分布按评审计数，`case*` 分布按唯一任务样本计数；
缺评、单评和边界分歧单列，不能把 24 份双评当成 24 个任务，也不能把分层样本当总体完成率。

`compare` 不需要数据库参数，只接受两个当前格式的 report JSON。它检查版本、范围、规则定义与阈值，并校验数值和分母关系。旧报告缺少必需字段时应重新运行 `report`，不要手工补零；工具未出现或分母为零时，相关比例保留空值。

当前可运行的项目检查：

```bash
cd side-projects/agent-defect-analyzer
bun run typecheck
bun test
```

测试使用临时 SQLite fixture，覆盖格式兼容、只读边界和 CLI 输出；不访问本机生产库。两个项目的本地检查不由根 Cargo workspace 测试替代。

## 迁移表

| 旧入口 | 迁移处理 | 统一 CLI 归属 |
| --- | --- | --- |
| `long_session_study.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report` |
| `agent_dispatch_study.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report` |
| `tool_token_consumption.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report`；字节量独立统计，实际 token 暂不可测 |
| `ratio_analysis.ts` | 移除活动入口；旧双窗口口径不复用 | `compare`（只比较兼容 report） |
| `wander.ts`、`export_sessions.ts` | 移除一次性导出入口 | `sample` / `evidence`（默认 metadata） |
| `timeline_study.ts`、`ultracode_prompts.ts` | 移除硬编码/一次性研究入口 | `report` 或历史化 |
| `optimization_chart.ts`、`tool_token_chart.ts`、`tool_token_charts.ts` | 移除硬编码图表生成器 | 统一报告产物 |

创建窗口变化只能支持描述性差异，不能解释为因果改善。具体复核流程见 [ANALYSIS.md](ANALYSIS.md)。

研究、eval 与 ADLC 的运行日志、审计快照和阶段报告保存在本地 `output/` 或按日期命名的 `reports/YYYY-MM-DD*` 中，不提交到仓库。测试必需的脱敏协议样本存放在 `src/research/fixtures/`。

## 历史产物

下列旧报告和本机可能留存的 `src/data/*.json` 是特定数据库快照的历史记录，不构成当前数据结论。后续报告通过统一入口生成，并包含来源指纹、范围、解析质量和分母；源数据库路径由运行者本地保留。

历史报告：

- [long-session-study.md](reports/long-session-study.md)
- [wander-report-2026-08-10.md](reports/wander-report-2026-08-10.md)
- [guide-peri-improvements.md](reports/guide-peri-improvements.md)

旧报告中引用的 `docs/`、`src/metrics/` 和 `scripts/wander.ts` 等路径已经不属于当前项目结构；不要按这些路径运行或补造文件。

## 数据安全

分析器只读打开 `~/.peri/threads/threads.db`。生产数据库不应由分析器写入；原始对话、工具参数和工具输出按需在本地查看，不提交到仓库。没有源数据时只能验证算法 fixture，不能声称重新验证历史数字。
