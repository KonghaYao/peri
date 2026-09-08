---
name: learn-from-history
description: >
  审计近期对话历史，把重复失败与成功模式转成可证伪的 harness 改进契约，并在后续审计中归因。
  用户要求总结历史对话、回顾近期 agent 表现、从历史中学习或寻找可自动化改进时使用。
---

# Learn From History

把历史学习当成一个**可观测改进环**，而不是经验摘抄：固定输入，分层下钻证据，定位最窄变更面，为每项建议同时声明预测收益与回归风险，再由后续历史验证。

默认审计当前项目最近 7 个自然日期（含今天），不跨项目。报告和 decision manifest 是审计产物，不构成编辑授权；不自动修改或回滚规则、skill、代码。

本流程采用 [Agentic Harness Engineering](https://arxiv.org/abs/2604.25850) 的三层可观测思想，并适配为有人确认的项目审计：

1. **组件可观测**：每个失败模式只归属一个首选变更面；
2. **经验可观测**：最终概览可下钻到 unit finding，再到固定 snapshot 的原始 thread；
3. **决策可观测**：每个变更建议都是带收益预测、回归风险和验收条件的可证伪契约。

## 事实源

- 运行编排与 unit prompt：`scripts/run_history.py`
- 提取逻辑：`scripts/extract_daily.py`
- run 与 decision manifest 校验：`scripts/validate_run.py`
- unit 报告格式：`references/analysis-template.md`
- 跨轮决策账本：`spec/reviews/history-learn-YYYY-MM-DD.json`

`extract_range.py` 仅保留手工范围导出的兼容用途，不是主路径。

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

它通过 SQLite backup 固定本次审计的数据边界，提取物权限为 `0600`。`manifest.json` 是本次运行的唯一输入清单，记录 snapshot digest、`repository_root`、日期、thread、消息数、输入 digest、降级统计和分析单元。

**完成标准**：命令 exit 0，manifest `status=ready` 或 `status=empty`。任一日期失败时命令必须 exit 非零；不得分析部分成功结果。`empty` 时报告近期无记录并结束。

### 2. 检查 manifest

Read `manifest.json`，核对：

- `project_filter` 或 `all_projects` 与用户范围一致；
- `window.active_days`、`totals.thread_count`、`totals.message_count`；
- `totals.truncations` 与 `totals.parse_failures`；
- 每个 `unit` 的输入、消息数、prompt、summary 和 sidecar 路径。

本流程按 thread 的 `updated_at` 日期归档**完整 thread**，不按消息切断因果链。报告中写清该语义。

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

- `summaries/unit-NNN.md`：thread 结果和跨 thread finding；
- `summaries/unit-NNN.json`：`status=analyzed`、输入 digest、覆盖数、finding 契约和降级复核。

优先比较相同或相近意图中的成功/失败轨迹，找出**分歧点**；它比单独阅读失败更能区分能力缺口、随机执行偏差和 harness 缺陷。每条 finding 的证据与反证必须写成：

```text
extracted/<day>/<thread>.txt :: <可定位摘录或事件>
```

原始 thread 是证据层，不是默认阅读入口；先读 unit finding，主张不足时再下钻。输入中有 `[TRUNCATED ...]` 或 `[MESSAGE_PARSE_FAILED]` 时，必须人工评估该 thread 是否仍足够支撑 finding，并在 `degraded_inputs_reviewed` 登记；证据不足则写入 `blocked`，不得外推。

### 4. 机器校验经验层

所有 unit 终态后运行：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id>
```

validator 检查：

- summary 非空且不是 `null`；
- sidecar unit ID、`status=analyzed`、thread 数和消息数；
- 输入文件集合与 manifest 完全相等，digest 未变化；
- 降级输入已显式复核，没有 blocked 输入或 extraction failure；
- finding 含 classification、failure pattern、root cause、可下钻 evidence/counterevidence、带分母 frequency、impact、confidence、fact source；
- finding 已选择 target surface，解释归属，并声明 predicted fixes、risk regressions 与 acceptance。

**完成标准**：命令 exit 0 且 `validation.json` 为 `passed`；其 `attestation` 记录本次实际读取的 manifest digest 与完整 sidecar `{unit_id, path, sha256}` 集合，供同日 decision ledger 复写并在后续审计中三方核对。失败 unit 优先 resume；校验通过前不得汇总或宣称完成。

### 5. 归因上轮决策

在汇总新建议前，按日期读取当前项目 `spec/reviews/` 中相关的 `history-learn-*.json`。只处理 `status=implemented`、含实施验证，且未被任何更新账本给出经保留的 `validation.json`、manifest digest 和 unit sidecar 共同证明的 `keep | revert` 终局 verdict 的变更；自报或已丢失 run 证据的终局不生效，`improve` 与 `inconclusive` 保持待观察。

对每个 prior change：

1. 将 `predicted_fixes` 与本轮观察到的改善逐项对照；
2. 主动检查 `risk_regressions`，并从旧有成功模式中选择至少一个 preserved-success probe；
3. 区分“改动后发生”与“由改动导致”；没有同类对照、明确分歧点或独立验收时，不声称因果；
4. 给出 `keep | improve | revert | inconclusive | not_implemented` verdict；
5. `revert` 只是建议，仍需用户确认，且必须说明恢复范围与保留哪些有效部分。

历史窗口未覆盖实施前基线、相关场景未再次出现、运行环境或模型改变时，verdict 必须是 `inconclusive`，不能用“未再出现失败”冒充修复成功。

### 6. 聚合、去重与组件归属

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

按**最窄有效层**选择一个首选 target surface：

| 失败根因 | 首选面 |
| --- | --- |
| 稳定工程约束缺失 | standard 或 module guidance |
| 产品行为/架构缺陷 | active issue，再落到 implementation + test |
| 可复用但按需触发的工作流缺失 | skill |
| 工具说明或 schema 让模型误用现有能力 | tool description |
| 工具能力、错误恢复或输出形态不足 | tool implementation |
| 需要跨步骤观察、拦截或完成门 | middleware |
| 需要隔离上下文或专门角色处理独立子任务 | subagent |
| 多轮重复出现且跨任务稳定的边界经验 | memory |
| 参数/注册/权限装配错误 | configuration |
| 已有规则未执行，且无结构性缺口 | none；记录 execution deviation |
| 外部平台或权限阻塞 | external |

不要默认把所有教训塞进 prompt、规则或本 skill。论文消融显示组件收益不相加，重复约束会增加冗余检查；若多个候选面表达同一防线，只保留执行力最强且副作用最小的一层，其他层仅在有独立证据时补充。

只有多次证据、影响明确且存在事实源缺口时才建议新稳定规则；单次事件默认不制度化。若可观测，记录消息数、重复工具调用、错误重试或耗时等效率代理，但不能以“更短”替代任务正确性。

### 7. 生成报告与 decision manifest

写入同日配对产物：

```text
spec/reviews/history-learn-YYYY-MM-DD.md
spec/reviews/history-learn-YYYY-MM-DD.json
```

Markdown 报告至少包含：

1. snapshot 截止时间、项目过滤和“按 thread updated_at 归日”语义；
2. 日期、thread、消息、unit、截断和解析失败统计；
3. prior change attribution 与 verdict；
4. finding 的根因、可下钻证据/反证、频次、影响、置信度与事实源；
5. 稳定规则候选、skill 候选、已有覆盖、成功模式；
6. validation 结果和 blocked 项；
7. 结构化 change plan。

JSON 是决策账本，至少包含：

```json
{
  "version": 1,
  "run_id": "<snapshot run id>",
  "source_run_dir": "/tmp/learn-from-history/<run_id>",
  "source_manifest_sha256": "<manifest.json sha256>",
  "source_sidecars": [
    {
      "unit_id": "unit-NNN",
      "path": "summaries/unit-NNN.json",
      "sha256": "<sidecar sha256>"
    }
  ],
  "project_filter": "<project root or null>",
  "prior_attribution": [
    {
      "source": "spec/reviews/history-learn-YYYY-MM-DD.json",
      "change_id": "CHG-001",
      "verdict": "keep|improve|revert|inconclusive|not_implemented",
      "rationale": "<为何该证据支持此 verdict；区分时序相关与因果>",
      "observed_fixes": [
        {
          "source_finding": "unit-NNN/F-NNN",
          "source_run_id": "<本轮 snapshot run id>",
          "source_manifest_sha256": "<本轮 manifest.json sha256>",
          "finding_contract": {
            "id": "F-NNN",
            "classification": "<本轮 finding classification>",
            "failure_pattern": "<本轮 finding failure_pattern>",
            "root_cause": "<本轮 finding root_cause>",
            "target_surface": "<本轮 finding target_surface>",
            "predicted_fixes": [],
            "risk_regressions": [],
            "acceptance": {"target": [], "preserved_success": []}
          },
          "finding_digest": "<finding_contract 的规范 SHA-256>",
          "prior_contract": "<旧 change.predicted_fixes 中的原文>",
          "outcome": "fixed|improved|unchanged|regressed|not_observed",
          "observed_delta": "<本轮观察到的脱敏变化>"
        }
      ],
      "observed_regressions": [
        {
          "source_finding": "unit-NNN/F-NNN",
          "source_run_id": "<本轮 snapshot run id>",
          "source_manifest_sha256": "<本轮 manifest.json sha256>",
          "finding_contract": {
            "id": "F-NNN",
            "classification": "<本轮 finding classification>",
            "failure_pattern": "<本轮 finding failure_pattern>",
            "root_cause": "<本轮 finding root_cause>",
            "target_surface": "<本轮 finding target_surface>",
            "predicted_fixes": [],
            "risk_regressions": [],
            "acceptance": {"target": [], "preserved_success": []}
          },
          "finding_digest": "<finding_contract 的规范 SHA-256>",
          "prior_contract": "<旧 change.risk_regressions 中的原文>",
          "outcome": "fixed|improved|unchanged|regressed|not_observed",
          "observed_delta": "<本轮观察到的脱敏变化>"
        }
      ]
    }
  ],
  "changes": [
    {
      "id": "CHG-001",
      "status": "proposed|implemented|blocked",
      "source_findings": ["unit-NNN/F-NNN"],
      "classification": "skill_gap",
      "failure_pattern": "<observable pattern>",
      "root_cause": "<causal hypothesis>",
      "baseline": "<当前 snapshot 中脱敏的发生率、成功率或具体现状>",
      "target_surface": "skill",
      "files": ["<repository-relative path>"],
      "why_this_surface": "<component choice>",
      "predicted_fixes": ["<next-run observable outcome>"],
      "risk_regressions": ["<preserved behavior at risk>"],
      "acceptance": {
        "target": ["<target check>"],
        "preserved_success": ["<preserved-success check>"]
      },
      "verification": []
    }
  ]
}
```

一个 logical change 对应一个 entry；不要把跨组件“大改造”打包成不可归因的一项。`baseline` 必须保留当前 snapshot 中脱敏、可比较的发生率或具体现状，因为 `/tmp` 原始输入清理后它是下轮归因的参照。`source_sidecars` 必须逐项复制本轮 `validation.json.attestation.sidecars`，与 `source_manifest_sha256` 一起把 canonical repository ledger 绑定到实际通过校验的 unit sidecar；旧 ledger 缺此字段时历史终局 fail closed，但该 change 仍可在新账本中重新归因。change 的 `classification`、`target_surface` 与 `acceptance` 不能脱离所引用 finding；可以追加检查，但不能省略 finding 已声明的检查。`acceptance.target` 与 `acceptance.preserved_success` 各至少一项，按折叠空白后的文本全局唯一。`proposed` 时 `verification` 为空；实施后每个 acceptance 恰好对应一个 `{check, command, status, result}`，且全部为 `passed`。prior attribution 的 observation 必须同时绑定当前 `source_finding`、本轮 run/manifest、`finding_contract` 及其规范 digest、旧 change 中逐字匹配的 `prior_contract`、受限 `outcome` 和本轮 `observed_delta`；`finding_contract` 取 validator 定义的核心 finding 字段。`keep` 至少需要 `fixed|improved` 且不能有 `regressed`，`revert` 至少需要 regression observation 的 `regressed` 且不能同时声称修复。不能用无关 finding 与自由文本拼出强 verdict。建议必须列出至少一个预测修复、一个回归风险或明确的 no-risk 理由，以及目标验收和 preserved-success 验收。报告和 JSON 都必须脱敏，不复制凭据、认证头、完整用户数据或本机私密配置。

生成后运行：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id> \
  --decision-manifest spec/reviews/history-learn-YYYY-MM-DD.json
```

如果校验的是旧 v1 `--all` run，manifest 可能没有 `repository_root` 且 `project_filter=null`；此时必须显式绑定账本所属仓库，不能从 decision 路径静默推断：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id> \
  --decision-manifest spec/reviews/history-learn-YYYY-MM-DD.json \
  --repository-root <工作目录>
```

**完成标准**：run 与 decision manifest 均为 `passed`，每个 change 都能追溯到当前 unit finding。

### 8. 分层确认后编辑

报告生成后必须询问用户，不能把模糊的“全部”跨作用域解释：

- **仅报告**：不改文件；
- **项目内稳定规则**：只改项目 standards/模块事实源；
- **项目内全部**：还可改项目级 skill、测试或 active issue；
- **包含用户级 skill**：单独明确授权后才可修改 `~/.claude/skills/`；
- **逐项确认**：按 change ID 选择。

新 skill、用户级文件、提交、push 和高影响 Git 操作永远不由“项目内全部”隐式授权。

编辑时保持一项 change 对应最小 diff。完成后：

1. 运行该项 `acceptance` 中的目标检查与 preserved-success 检查；
2. 将 decision manifest 的 `status` 改为 `implemented`；`verification` 为 `acceptance.target` 与 `acceptance.preserved_success` 的每个检查写一个 `{check, command, status, result}` 对象，`check` 必须与 acceptance 原文完全一致，且全部为 `passed`；未实施保持 `proposed`，受阻写 `blocked`；
3. 再次运行 decision manifest 校验；
4. 不 commit，除非用户另行明确要求。

若多个 change 同时落地且作用面重叠，下一轮无法可靠单项归因；优先分批实施或在报告中显式标记 confounded。

### 9. 清理敏感输入

最终报告和 decision manifest 写完、全部校验通过后，默认清理 snapshot、原始提取物和 prompts：

```bash
python3 .claude/skills/learn-from-history/scripts/validate_run.py \
  /tmp/learn-from-history/<run_id> \
  --decision-manifest spec/reviews/history-learn-YYYY-MM-DD.json \
  --cleanup-inputs
```

保留 manifest、`validation.json`、summary sidecar 和脱敏报告/决策账本；它们共同构成后续终局 attribution 的证据链。历史 attestation 必须从限定 run/repository 根下以不跟随 symlink 的普通文件读取，并让解析内容与 digest 来自同一次打开；canonical repository ledger 的 `source_sidecars`、旧 `validation.json.attestation` 与保留 sidecar digest 必须逐项一致。路径异常、换指、产物缺失或 digest 不符时一律 fail closed。此时旧 `keep|revert` 不得关闭变更，下一轮继续归因。该链提供可审计的一致性和 Git 可追踪锚，不宣称能抵抗可同时重写仓库 ledger、Git 历史与 `/tmp` 产物的主体；报告和 ledger 仍需人工确认。若用户明确需要保留原始审计输入，跳过 cleanup 并提示其敏感性和路径。

## 失败处理

| 状态 | 行动 |
| --- | --- |
| 数据库不存在或 snapshot 失败 | 报告阻塞并结束 |
| manifest `empty` | 报告近期无记录，可询问是否 `--all` |
| manifest `failed` 或命令非零 | 不启动 agent；修复或重新创建 run |
| agent 中断 | resume 原 child thread |
| sidecar 缺失、digest 不符、覆盖不全 | validator 失败；不得汇总 |
| finding 无原始路径 locator、根因或反证检查 | validator 失败；补证据，不降格为直觉建议 |
| 输入截断/解析失败且无法复核 | 标为 blocked，不将相关判断写成稳定规则 |
| prior change 缺基线或相关场景未复现 | `inconclusive`，不判 keep/revert |
| 回归风险未搜索 | 不实施；补 preserved-success probe |
| 事实源已有同义规则 | 标记已覆盖或仅强化原 Verify |
| 建议涉及用户级 skill | 单独确认，不继承项目内编辑授权 |
