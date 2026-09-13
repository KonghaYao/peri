# Agent Defect Analyzer

Peri 会话数据的只读分析项目。它的目标是把可复核的观测转成 agent 改进候选；统计结果不能直接证明任务失败、用户满意度或修复因果。

分析口径、证据要求和禁止推断见 [ANALYSIS.md](ANALYSIS.md)。数据库 schema 与消息格式以当前 Peri 代码和测试为准，历史报告只作为调查线索，不能代替重新取数。

## 当前状态

统一 CLI 的 `inspect` 只读检查数据库结构、消息格式、工具调用配对和数据质量，生成 `quality.json` 与 `quality.md`。历史工具事实包含当前被 compact 排除的消息；子会话自己持久化的消息属于自己的工作，继承快照单独计数。

```bash
cd side-projects/agent-defect-analyzer
bun install --frozen-lockfile
bun run inspect --db ~/.peri/threads/threads.db --out output/quality
bun run report --db ~/.peri/threads/threads.db --out output/behavior
# 指定会话创建窗口，UTC 半开区间
bun run report --db ~/.peri/threads/threads.db --since 2026-09-01T00:00:00Z --until 2026-09-13T00:00:00Z --out output/recent
```

报告默认不含对话正文、工具参数和文件路径。异常证据保留会话与消息 ID，便于在本机复核。`output/` 是被忽略的本地产物目录。缺失能力、未知格式和解析错误必须查看，不能只读总数。

`report` 输出 `report.json` 与 `report.md`，默认分析可见主会话自己持久化的历史。可选 `--scope roots|children|all`、`--include-hidden`；子会话通常隐藏，分析子会话时显式加 `--include-hidden`。时间筛选依据会话创建时间，纳入该会话的全部持久化消息，不能解释为窗口内发生的工具事件。

错误率的分母是已配对且明确记录成功/错误状态的结果。重复调用与连续失败规则输出达到阈值的候选位置数，不能解释为已经确认的缺陷数量。JSON 包含有界的 `threadId/messageId/callId` 证据与 `nextVerification`；按证据复核后再创建修复任务。

当前可运行的项目检查：

```bash
cd side-projects/agent-defect-analyzer
bun run typecheck
bun test
```

测试使用临时 SQLite fixture，覆盖格式兼容、只读边界和 CLI 输出；不访问本机生产库。两个项目的本地检查不由根 Cargo workspace 测试替代。

## 迁移表

| 旧入口 | 阶段 1 处理 | 统一 CLI 归属 |
| --- | --- | --- |
| `long_session_study.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report` |
| `agent_dispatch_study.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report` |
| `tool_token_consumption.ts` | 移除活动入口；历史 JSON/报告保留 | `inspect` / `report`；字节量独立统计，实际 token 暂不可测 |
| `ratio_analysis.ts` | 移除活动入口；双窗口研究待重建 | `compare`（阶段 3） |
| `wander.ts`、`export_sessions.ts` | 移除一次性导出入口 | `sample` / `evidence`（阶段 3） |
| `timeline_study.ts`、`ultracode_prompts.ts` | 移除硬编码/一次性研究入口 | `report` 或历史化 |
| `optimization_chart.ts`、`tool_token_chart.ts`、`tool_token_charts.ts` | 移除硬编码图表生成器 | 统一报告产物（阶段 2） |

当前提供 `inspect` 和 `report`。`compare`、`sample`、`evidence` 随后接入。

## 历史产物

下列旧报告和本机可能留存的 `src/data/*.json` 是特定数据库快照的历史记录，不构成当前数据结论。后续报告通过统一入口生成，并包含快照指纹、范围、生成时间、解析质量和分母；源数据库路径由运行者本地保留。

历史报告：

- [long-session-study.md](reports/long-session-study.md)
- [wander-report-2026-08-10.md](reports/wander-report-2026-08-10.md)
- [guide-peri-improvements.md](reports/guide-peri-improvements.md)

旧报告中引用的 `docs/`、`src/metrics/` 和 `scripts/wander.ts` 等路径已经不属于当前项目结构；不要按这些路径运行或补造文件。

## 数据安全

分析器只读打开 `~/.peri/threads/threads.db`。生产数据库不应由分析器写入；原始对话、工具参数和工具输出按需在本地查看，不提交到仓库。没有源数据时只能验证算法 fixture，不能声称重新验证历史数字。
