# Workflow 历史执行审计（2026-08-19～2026-09-01）

## 1. 审计范围与方法

- **范围**：最近 14 个自然日期（含 2026-09-01），跨所有项目；其中 13 天存在满足条件的活跃 thread。
- **快照**：固定 SQLite 副本；按 manifest 切分 7 个分析单元，不以关键词筛选代替 thread 覆盖。
- **覆盖**：203/203 threads、44,896/44,896 条消息；7/7 sidecar 通过 `validate_run.py`，无 blocked 输入。
- **降级证据**：提取器记录 12,645 处内容截断、59 次消息解析失败；相关输入已逐单元人工复核，仅保留仍可核对的事件和产物，不从缺失正文外推。
- **项目分布**：主要集中于 `perihelion`（85）、`rcs-yjs-squash`（28）、`remote-control-server`（20）、`peri-studio`（19）、`tavily-search`（19），其余为示例、临时 E2E 目录及多个独立项目。
- **机器 finding**：34 项，含 `rule_gap` 10、`execution_deviation` 11、`active_issue_covered` 4、`skill_gap` 4、`external_blocker` 5；本报告已去重，不将 34 项机械转换为 34 个需求。

## 2. Workflow 运行概览

在 manifest 范围内，17 个 thread 出现可核对的 Workflow 终态通知。按 `run_id` 去重后：

| 指标 | 结果 | 解释 |
|---|---:|---|
| 唯一终态 run | 66 | 从终态通知及 state 路径去重 |
| `completed` | 56（84.8%） | 仅代表 engine 终态，不等于业务交付通过 |
| `failed` | 10（15.2%） | 其中 3 个为 0 agent/0 tool-call 的启动前失败 |
| 累计 agent | 317 | 平均 4.8/run |
| 累计 tool calls | 22,490 | 平均 340.8/run |
| 累计 wall-clock | 约 29.6 小时 | 平均 26.9 分钟/run；并行 phase 时长不可当作串行人工工时 |
| 高成本 run | 24/66 | 条件：agents≥8、tool calls≥500 或 duration≥30 分钟，满足其一 |
| 失败但已执行工作 | 7/10 | 合计 3,194 tool calls；失败不代表“没有产生改动/可消费结果” |

最高可见单 run 为 12 agents、1,820 tool calls；另有 9-agent run 达 1,287 calls，11-agent failed run 达 1,137 calls。通知已提供 duration、agent 数、tool calls，部分通知还提供 phase token；但没有统一的预算消耗率、cache/reuse 或货币成本，因此本审计不估算美元成本。

## 3. 总体判断

Workflow 的主要价值已经成立：复杂任务中，并行探索、独立 Review、验证角色证伪和恢复原线程，确实发现了单 Agent 容易遗漏的回归、TOCTOU、状态机和跨层契约问题。核心调度、异步通知、state/journal 持久化及失败传播也有大量成功对照。

当前主要风险不在“能不能跑”，而在三个边界：

1. **engine 终态与业务交付终态混在一起**：`completed` 后仍可能有 P1、视觉偏差、未归属改动或未提交内容；`failed` 也可能是验收已通过、只在 post-processing 超时。
2. **写入归属和结果真实性依赖主线程补救**：存在 agent 报告已改但未写盘、工作树污染、夹带既有 staged 文件、误报已提交、cwd 错误生成根 manifest 等证据。
3. **高成本 run 缺预算和收敛控制**：已具备基础遥测和底层 `budgetTotal` wire，但 `Workflow` 工具 schema 把 `budget_total` 固定为 `None`，`ultracode` 也未要求预算/早停；长任务容易跨多次 compact 继续扩展范围。

因此不建议先简单减少 agent 数。应先建立可交付门禁、预算与阶段语义，保留“并行探索 + 独立验证”的质量收益。

## 4. 优化建议

### P0 — 分离执行、验收、收口和交付状态

**证据**：

- 2/4 个显式 Workflow 样本出现 `completed` 后仍有 P1 或结果范围漂移。
- 一个 11-agent、1,137-call run 最终标 `failed`，但核心 E2E 已 28/28 passed，失败来自后置收口。
- 7/10 failed run 已实际执行 agent/tool calls，可能已产生改动或可消费阶段结果。
- `TEST-EVIDENCE-001` 已规定 completed 通知本身不算通过，但 Workflow 结果契约仍只有一个总状态。

**建议**：在 state、通知和 TUI 面板中分别保存：

- `execution_status`
- `acceptance_status`
- `post_processing_status`
- `delivery_status`

总状态只做派生展示；失败摘要列出已完成阶段、可消费产物、dirty files 和可恢复阶段。`completed` 只有在独立验收、结果消费和工作树归属检查通过后才能映射为 `deliverable`。

**验收**：

1. 构造“执行成功、验收成功、post-processing 超时”fixture，结果不得笼统标成全部失败；只恢复未完成阶段。
2. 构造“engine completed、独立验收发现 P1”fixture，`delivery_status` 必须保持 blocked。
3. 构造“failed 但已有部分改动”fixture，通知必须列出已完成 phase、changed files 与恢复入口。

**事实源**：建议新增 active issue；稳定测试证据继续由 `docs/standards/testing.md#TEST-EVIDENCE-001` 承载，不复制到根路由文件。

### P0 — 写入型 Workflow 增加工作树归属与 postcondition

**证据**：

- 已有 Open issue 记录 Workflow agent 声称修改代码但实际未写盘。
- 本窗口又出现异常未跟踪文件、覆盖关键行为测试、前后 git 状态无法归属、提交夹带既有 staged 文件、截断输出导致误报已提交、错误 cwd 在仓库根生成 manifest/lockfile。

**建议**：

- 启动前记录 baseline：repo root、HEAD、index/worktree 状态、允许修改的路径/仓库。
- 每个写入 agent 记录 declared changed-files，并与实际 diff 对账；跨 repo、越界文件或覆盖既有 dirty 内容时阻止自动消费。
- Git 收口不解析自然语言/截断输出，直接核对 HEAD、index、worktree、commit file list 和预期 hash。
- step 执行前校验 cwd 与预期 manifest；不匹配时在产生文件前失败。

**验收**：用含预先 dirty/staged 文件、多 repo、错误 cwd 和 agent 自报假改动的 fixture，证明越界改动不进入交付，用户既有改动不被覆盖，提交文件集合与 allowlist 完全一致。

**事实源**：强化 `spec/issues/2026-07-22-workflow-agent-hallucinated-code-changes.md`，不要重复新建同义 issue。

### P0 — 暴露预算并加入阶段级早停

**证据**：

- 24/66 run 达到高成本条件；最高 1,820 tool calls；失败 run 仍累计 3,194 calls。
- 多个超长 thread 经多次 compact、数十个后台终态后仍未闭环。
- Rust/Node wire 和 engine 已支持 `budgetTotal`，但 `Workflow` tool schema 没有该字段，`tool.rs` 固定 `budget_total: None`。

**建议**：

1. 先把已有能力打通：tool schema 暴露 `budgetTotal`，`ultracode` 要求大型 workflow 启动前设置预算。
2. 增加 `maxElapsedMs`、`maxToolCalls`、`maxAgents`、`maxCompactions` 和阶段预算；阈值到达时暂停并请求主线程决定，不默认继续扩编。
3. 增加“连续无新增 confirmed finding”“相同验证重复失败”“范围扩展未获授权”等早停条件。
4. 通知展示 `used/budget`、超限原因、cache hit/resume reuse 和产出/成本比；预算不能只做总 token 单一数字。

**验收**：对固定大型 fixture，达到每种阈值都能停止且保留 state；resume 后只运行未完成阶段；不重复已完成写操作。对 10 个超阈值历史同型任务，100% 输出 done/partial/blocked 终态矩阵。

**事实源**：建议新增 Workflow budget/early-stop active issue；`ultracode` 为 builtin、当前无可编辑磁盘源，skill 修改需由 builtin 上游实现，避免项目级同名 override 遮蔽完整行为。

### P1 — 无副作用 preflight/compile

**证据**：2/35 个有明确终态的单元样本在 22–36ms、0 agents/0 calls 时因脚本解析失败；缩小脚本后人工恢复成功。另有 provider/tier 401、400、404 需在执行前区分配置阻塞。

**建议**：启动前单独执行：

- JS parse/type/primitive validation；
- phase graph、必填 return 和结果引用检查；
- model tier/provider 可用性检查；
- cwd、scriptPath、manifest 与 repo boundary 检查；
- 预算和最大并发合法性检查。

preflight 失败不注册 Running run、不产生写入；错误需定位阶段和行。预期失败 smoke 仍允许显式跳过 preflight 或声明 expected failure。

**验收**：语法、未知 primitive、不可用 tier、错误 cwd fixture 全部在 0 agent/0 tool-call 且无新增文件时失败，并返回结构化错误码。

### P1 — 结构化结果链与唯一 attempt 身份

**证据**：

- 中间审查误报曾被验证角色证伪，若只消费前者会制造高风险误判。
- 截断结果、恢复历史和阶段终态有时只存在自然语言；大型后台 thread 的短 ID 重复显示，失败 attempt 与恢复关系难汇总。
- 成功对照表明：通知驱动、不轮询、恢复原 child thread、主线程明确消费结果，是应保留的模式。

**建议**：

- finding 保存 `proposed → confirmed/rejected` disposition、提出者、验证者和证据引用。
- 每个 task/run/attempt 使用完整唯一 ID，记录 parent orchestration、attempt、`recovered_from`、`consumed_at`、status、duration、tool calls、token usage。
- 最终汇总必须列出每个子任务首次状态、恢复次数、最终状态、是否消费；截断输出不得直接形成 confirmed finding。

**验收**：故障注入“审查误报后被证伪”和“流中断后 resume”两类场景，最终报告不得保留 rejected finding，也不得重复执行已完成写操作；所有事件可按 ID 一一关联。

## 5. 已覆盖项与非 Workflow 阻塞

| 发现 | 处理 |
|---|---|
| deferred Workflow 搜索不可见 | 已由 `spec/issues/2026-08-15-workflow-deferred-tool-missing.md` 修复，不重复建项 |
| Workflow agent 自报修改但未写盘 | Open issue `spec/issues/2026-07-22-workflow-agent-hallucinated-code-changes.md`，本轮补强验收 |
| 启动握手/错误传播/不存在 state 路径 | Partial issue `spec/issues/2026-08-01-workflow-tool-zero-agents-acp-bridge.md`，继续按原 issue 收口 |
| Langfuse subagent batch orphan | 已修复但待真实 trace 验证，保留原 issue |
| provider 401/400/404、外部 429/timeout | 外部/配置 blocker；preflight 与有限恢复可改善体验，不归因于 Workflow 调度缺陷 |
| 后台 completion/loading 状态机 | 有失败证据也有正常通知对照；应扩展 shell/agent/workflow E2E，不泛化为通知链完全失效 |
| 多工具同批副作用冲突 | 跨工具执行器架构风险，不是 Workflow 专属；建议单独进入 tool-dispatch active issue |
| ToolSearch 动态索引 stale | 动态工具目录风险，不是 Workflow run 本体；按 Tool Search 事实源处理 |

## 6. Skill 与审计基础设施改进

### P1 — 修正 Langfuse token/cost 语义

`build_usage_details()` 写出的 `usageDetails.input` 已是不含 cache 的 raw input；当前 `langfuse/scripts/lib.ts` 又以 `input - cacheRead` 计算 effective/cost，可能重复扣减，并且 pricing 未单独建模 cache creation。线上 Langfuse 当前不可达，本轮不能用真实账单最终确认。

建议用固定 fixture 对齐 tracer、Langfuse API 和 provider 账单语义；明确 raw input/cache create/cache read/output 各计一次，再用真实只读 trace 验证。该项先修观测口径，否则 Workflow 成本治理会建立在错误数据上。

### P1 — 修复 `learn-from-history` WAL 快照回归

本轮默认 `run_history.py --days 14 --all` 对 WAL-mode 源库执行 `backup()` 后，以 `mode=ro` 查询快照时报 `unable to open database file`。源库 quick check 正常；将独立副本切换为 DELETE journal mode 后，标准脚本成功创建固定 run。

建议在备份后显式完成 checkpoint/journal 转换，或以适用于不可变副本的只读方式打开；新增“WAL 源库 → 私有快照 → 只读聚合”回归测试。不得退化为直接分析活跃数据库。

## 7. 推荐实施顺序

1. **P0：交付状态机 + 工作树 postcondition**——先解决“完成但不可交付”和写盘/归属不可信。
2. **P0：预算暴露 + 阶段早停**——复用现有 `budgetTotal` wire，再补多维阈值和使用率。
3. **P1：preflight**——消除 0-agent 启动失败并提前暴露 provider/cwd/schema 阻塞。
4. **P1：结构化 attempt/result/disposition**——让失败恢复、成本和结果消费可追踪。
5. **观测修正**——先修 Langfuse token 口径，再建立成本 SLO；补真实 trace 验收。
6. **审计基础设施**——修复 WAL 快照路径，确保后续 14 天审计可重复运行。

## 8. 结构化 Change Plan

| ID | 目标文件/事实源 | 作用域 | 风险 | 操作类型 |
|---|---|---|---|---|
| WF-01 | 新建 `spec/issues/` Workflow delivery-state issue；后续涉及 `peri-acp-types/src/workflow.rs`、`peri-workflow/src/{tool,registry,runner}.rs` | 项目内产品契约与实现 | 高：跨 Rust/Node/ACP/TUI 状态语义 | 新 active issue + 代码/测试 |
| WF-02 | 强化 `spec/issues/2026-07-22-workflow-agent-hallucinated-code-changes.md`；后续涉及 Workflow journal/runner 与 Git postcondition helper | 项目内既有 issue | 高：工作树和用户改动安全 | 强化既有 issue + 代码/测试 |
| WF-03 | 新建 `spec/issues/` Workflow budget/early-stop issue；后续涉及 `peri-workflow/src/tool.rs`、wire types、Node server/engine adapter | 项目内工具 schema 与运行时 | 中高：终止/恢复/兼容性 | 新 active issue + 代码/测试 |
| WF-04 | 同 WF-03 或独立 preflight issue；涉及 Workflow tool/runner 与 npm package tests | 项目内启动路径 | 中：需保留 expected-failure smoke | 新 active issue + 代码/测试 |
| WF-05 | 新建结果链/attempt telemetry issue；涉及 Workflow、TaskManager、通知与 Langfuse 投影 | 跨模块可观测性 | 高：跨层序列化与身份契约 | 新 active issue + 代码/测试 |
| OBS-01 | `.claude/skills/langfuse/scripts/lib.ts` 及 fixtures/tests | 项目级 skill | 中：错误口径会污染成本决策 | Skill 修复 + 测试 + 真实 trace 验收 |
| HIST-01 | `.claude/skills/learn-from-history/scripts/run_history.py` 及 Python tests | 项目级 skill | 中：快照错误会阻塞全部历史审计 | Skill 修复 + WAL 回归测试 |
| BUILTIN-01 | builtin `ultracode` 上游源 | builtin skill，非当前仓库可直接编辑 | 中：项目级同名 override 会遮蔽内建行为 | 上游 skill 改进；当前 blocked |

本报告阶段不执行上述 change plan。后续应用时需按项目内规则、项目级 skill、builtin skill 分层确认；不能用一次“全部”跨越授权边界。

## 9. 结论

过去 14 天的 Workflow 不是“普遍失败”，而是“调度能力强、质量收益真实，但交付语义和成本治理滞后”。最有效的优化不是减少 agent，而是把以下链路做成强制契约：

`preflight → budgeted execution → independent acceptance → worktree postcondition → explicit delivery status → resumable structured result`

本报告仅记录建议，未修改 workflow、规则、issue 或 skill，未 commit。
