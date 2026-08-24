---
name: learn-from-history
description: >
  审计近期对话历史，提炼重复失败、成功模式、稳定规则和 skill 改进。用户要求总结历史对话、
  回顾近期 agent 表现、从历史中学习或寻找可自动化改进时使用。
---

# Learn From History

把历史学习当成一次**快照审计**：固定输入、按 manifest 分析、机器校验覆盖、去重后再建议修改。
默认审计当前项目最近 7 个自然日期（含今天），不跨项目，不自动修改任何规则或 skill。

## 事实源

- 运行编排：`scripts/run_history.py`
- 提取逻辑：`scripts/extract_daily.py`
- 完成校验：`scripts/validate_run.py`
- 人类日报格式：`references/analysis-template.md`

`extract_range.py` 仅保留手工范围导出的兼容用途，不是本 skill 主路径。

## 流程

### 1. 创建 snapshot run

从环境中的 Working directory 取得项目根，显式传入 `--cwd`：

```bash
python3 .claude/skills/learn-from-history/scripts/run_history.py \
  --days 7 \
  --cwd <工作目录>
```

只有用户明确要求跨项目时才使用 `--all`：

```bash
python3 .claude/skills/learn-from-history/scripts/run_history.py --days 7 --all
```

脚本创建权限为 `0700` 的唯一目录：

```text
/tmp/learn-from-history/<run_id>/
  manifest.json
  snapshot/threads.db
  extracted/<day>/*.txt
  prompts/unit-NNN.txt
  summaries/
```

它通过 SQLite backup 固定同一次审计的数据边界，提取物权限为 `0600`。`manifest.json` 是本次运行的唯一清单，记录 snapshot digest、日期、thread、消息数、输入 digest、降级统计和分析单元。

**完成标准**：命令 exit 0，manifest `status=ready` 或 `status=empty`。任一日期失败时命令必须 exit 非零；不得分析部分成功结果。`empty` 时报告近期无记录并结束。

### 2. 检查 manifest

Read `manifest.json`，核对：

- `project_filter` 或 `all_projects` 与用户范围一致；
- `window.active_days`、`totals.thread_count`、`totals.message_count`；
- `totals.truncations` 与 `totals.parse_failures`；
- 每个 `unit` 的输入、消息数、prompt、summary 和 sidecar 路径。

本流程按 thread 的 `updated_at` 日期归档**完整 thread**，不按消息切断因果链。报告中将该语义写清楚。

不要扫描 run 目录猜测输入，也不要读取其他 run 的同名文件。

### 3. 执行分析单元

每个 unit 的完整任务已经写入 `prompts/unit-NNN.txt`。派发 `general-purpose` agent 时，把该 prompt 文件内容作为任务；子 agent 自己直接 Read/Write，不得再次调用 Agent，不得修改仓库。

调度规则：

- 1 个 unit：同步执行；
- 2 个以上独立 unit：可后台并行，最多 3 个；
- 超过 3 个：分批启动，当前批次全部收到终态后再启动下一批；
- agent 失败时优先 resume 原 child thread，不创建重复任务；
- background 的 started/completed 通知不是通过证据，不轮询未完成结果。

单元按 thread 文件大小和数量规划，不机械按天切分；大日期可拆成多个 unit，小日期可合并。每个 agent 必须同时写：

- `summaries/unit-NNN.md`：人类可读分析；
- `summaries/unit-NNN.json`：`status=analyzed`、输入 digest、覆盖数、finding 证据与验收条件。

输入中有 `[TRUNCATED ...]` 或 `[MESSAGE_PARSE_FAILED]` 时，agent 必须人工评估该 thread 是否仍足够支撑 finding，并在 sidecar 的 `degraded_inputs_reviewed` 中登记；证据不足则写入 `blocked`，不得外推。

### 4. 机器校验完成性

所有 unit 终态后运行：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id>
```

validator 检查：

- summary 非空且不是 `null`；
- sidecar unit ID、`status=analyzed`、thread 数和消息数；
- 输入文件集合与 manifest 完全相等；
- 每个文件 digest 未变化且状态为 `analyzed`；
- 降级输入已经显式复核；
- finding 含 classification、evidence、counterevidence、frequency、impact、confidence、fact source 和 acceptance；
- 没有 blocked 输入或 extraction failure。

**完成标准**：命令 exit 0 且 `validation.json` 为 `passed`。失败 unit 优先 resume；校验通过前不得进入汇总或宣称完成。

### 5. 汇总与去重

只读取当前 manifest 列出的 unit summary/sidecar。每条 finding 先分类：

- `rule_gap`：真实稳定规则缺口；
- `active_issue_covered`：已有 active issue，禁止复制事故叙事；
- `skill_gap`：现有 skill 缺指引或触发失败；
- `execution_deviation`：规则已覆盖但未遵循；
- `external_blocker`：环境、权限、provider 或平台阻塞。

再读取当前项目根路由和 finding 所需的最小事实源：

- 根 `CLAUDE.md`：只判断路由，不写工程规则正文；
- `docs/standards/` 与测试 canonical standard：稳定规则；
- 对应模块 `CLAUDE.md`：模块入口和专属不变量；
- `spec/issues/`：active change、事故验收和具体产品风险；
- `spec/global/problems.md`：历史索引；
- `DiscoverSkillsTool`：当前 skill catalog。

每条候选标注：新增、强化现有项、active issue 已覆盖、仅执行偏差或 blocked。只有多次证据、影响明确且存在事实源缺口时才建议新规则；单次事件默认不制度化。

### 6. 生成报告

写入 `spec/reviews/history-learn-YYYY-MM-DD.md`，至少包含：

1. snapshot 截止时间、项目过滤和“按 thread updated_at 归日”语义；
2. 日期、thread、消息、unit、截断和解析失败统计；
3. finding 的 evidence、counterevidence、带分母频次、影响、置信度、事实源与 acceptance；
4. 稳定规则候选、skill 候选、skill 改进、已有覆盖、成功模式；
5. validation 结果和 blocked 项；
6. 结构化 change plan：ID、目标文件、作用域、风险、操作类型。

报告必须脱敏，不复制凭据、认证头、完整用户数据或本机私密配置。

### 7. 分层确认后编辑

报告生成后必须询问用户，不能把模糊的“全部”跨作用域解释：

- **仅报告**：不改文件；
- **项目内稳定规则**：只改项目 standards/模块事实源；
- **项目内全部**：还可改项目级 skill、测试或 active issue；
- **包含用户级 skill**：单独明确授权后才可修改 `~/.claude/skills/`；
- **逐项确认**：按 change plan ID 选择。

新 skill、用户级文件、提交、push 和高影响 Git 操作永远不由“项目内全部”隐式授权。编辑后运行目标测试与 `git diff --check`，不 commit，除非用户另行明确要求。

### 8. 清理敏感输入

最终报告写完且 validation 已通过后，默认清理 snapshot、原始提取物和 prompts：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id> \
  --cleanup-inputs
```

保留 manifest、validation、summary sidecar 和脱敏报告。若用户明确需要保留原始审计输入，跳过 cleanup 并提示其敏感性和路径。

## 失败处理

| 状态 | 行动 |
| --- | --- |
| 数据库不存在或 snapshot 失败 | 报告阻塞并结束 |
| manifest `empty` | 报告近期无记录，可询问是否 `--all` |
| manifest `failed` 或命令非零 | 不启动 agent；修复或重新创建 run |
| agent 中断 | resume 原 child thread |
| sidecar 缺失、digest 不符、覆盖不全 | validator 失败；不得汇总 |
| 输入截断/解析失败且无法复核 | 标为 blocked，不将相关判断写成稳定规则 |
| 事实源已有同义规则 | 标记已覆盖或仅强化原 Verify |
| 建议涉及用户级 skill | 单独确认，不继承项目内编辑授权 |
