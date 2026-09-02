# 历史学习报告（2026-08-17～2026-08-24）

## 1. 扫描概览

- **项目过滤**：`/Users/konghayao/code/ai/perihelion`（前缀匹配，因此包含 `example/` 子目录；没有混入仓库外项目）
- **自然日期**：8 天，2026-08-17～2026-08-24
- **Thread**：102/102，全部读取
- **消息**：16,430 条，全部纳入日报分析
- **初始查询差异**：活跃日期查询时为 16,426 条；本次 `/learn-from-history` thread 在查询后继续增加 4 条，因此最终提取数多 4 条，不是遗漏
- **仓库状态**：汇总前仅观察到既存 `peri-cool` submodule 改动；本流程未触碰该改动

| 日期 | Thread | 消息 | 主要主题 |
|---|---:|---:|---|
| 2026-08-17 | 21 | 2,029 | Langfuse、ACP/stdio、发布与跨平台测试 |
| 2026-08-18 | 14 | 2,272 | ACP commands、协议兼容、CPU/并发、DashScope tool ID |
| 2026-08-19 | 8 | 589 | EXDEV、rename、provider、Langfuse |
| 2026-08-20 | 2 | 414 | ACP continuation、transport 完成边界 |
| 2026-08-21 | 10 | 3,765 | MCP/MCPP 持久缓存、状态语义、ACP metadata |
| 2026-08-22 | 27 | 4,547 | 工具能力事实源、MetaHarness、stdio 动态更新、PTC、文档 |
| 2026-08-23 | 12 | 1,409 | PTC npm runtime、Windows、built-in subagent policy |
| 2026-08-24 | 8 | 1,405 | Windows CI 证据纪律、MCP Agents、self-build、TUI |

## 2. 结论摘要

最近八天最稳定的失败模式不是“缺少测试”，而是**验证证据没有覆盖用户实际观察到的生命周期**：

1. 过滤测试实际执行 0 个用例，仍被当作通过；测试输出经 `| tail`/`| grep` 后，退出码可能被掩盖。
2. 后台任务或 workflow 尚未收敛、阶段结果为 `null` 或输出被截断时，过早宣布质量门完成。
3. 动态能力只验证首发通知，不验证 discovery 后第二事件；持久缓存只验证进程内，不验证退出后新进程命中。
4. 本机测试、cross-target compile/clippy、目标平台 runtime 的证明能力被混用。
5. 禁用 middleware 或 built-in provider 时，只关闭一个注册面，slash route、静态 prompt、deferred index 等旁路仍暴露能力。

其中第 4 项已经由 active issue `spec/issues/2026-08-24-platform-ci-evidence-discipline.md` 完整记录。本报告不复制事故叙事，也不建议把它写入根 `CLAUDE.md`；应先按该 issue 连续验收，成熟后再提炼到 `docs/standards/`。

## 3. 建议新增或强化的规则

### P0 — TEST-EVIDENCE-001：验证命令必须证明自己执行了目标检查

**状态**：新增候选，建议写入 `docs/standards/testing.md`，不写根 `CLAUDE.md`。

**跨日证据**：

- 2026-08-17 `01a00ecd-6ad`：过滤测试出现 0 tests，局部结果不能证明目标路径。
- 2026-08-18 `01a013cf-1e5`、`01a01410-24d` 等长 thread：多次 `cargo test/clippy ... | tail`，pipeline 可能掩盖真实退出码。
- 2026-08-21 `01a023b3-c90`：在全量测试仍运行时宣布未见失败，随后用户提供 `1449 passed, 1 failed`。
- 2026-08-22 `01a024f8-e0b`：workflow `completed` 但阶段结果可能 `null`、verifier 输出可能截断。

**建议规则文本**：

```markdown
### TEST-EVIDENCE-001

- **Scope**：测试、lint、build、workflow 与后台验证任务。
- **Rule**：验证结论必须来自最终退出状态和可核对结果。过滤测试须确认实际执行非零目标用例；不得用未启用 `pipefail` 的 `| tail`、`| grep` 等 pipeline 作为通过证据；后台任务、workflow 和 CI 必须等待终态，并核对阶段结果非空、未截断。预期失败用例按验收目标分类，不按输出中的 `error` 字样机械判失败。
- **Verify**：报告精确命令、exit status、实际执行用例数或结构化 verdict；所有检查标注 passed / skipped / blocked，started 或 completed 通知本身不算通过。
```

### P0 — TEST-HERMETIC-001：测试不得读取用户持久状态或真实墙钟

**状态**：强化候选。现有 testing standard 已要求确定性并推荐 `tempfile`/`temp-env`，但没有明确禁止默认读取用户 HOME/cache/config。

**跨日证据**：

- 2026-08-18 并发基准最初被进程启动方式和顶层副作用污染。
- 2026-08-21 `01a023b3-c90`：MCP 测试读取用户持久 cache，导致全量套件出现 1 个真实失败。
- 2026-08-22 `01a024f8-e0b`：碰撞测试依赖真实墙钟，跨 epoch 后合法得到不同次数，被误判为生产回归。
- 2026-08-23 PTC 测试后期改用临时 HOME 和固定 package fixture，成为正确模式。

**建议规则文本**：

```markdown
### TEST-HERMETIC-001

- **Scope**：访问 HOME、cache、配置、凭据、时间、网络或进程级全局状态的测试。
- **Rule**：测试必须使用临时 HOME/cache/config 和固定时钟；不得默认读取开发者 `~/.peri`、真实凭据、持久 cache 或外部网络。修改进程级环境变量或全局状态时必须隔离并串行化。时间窗口、TTL、epoch、backoff 与碰撞测试使用可控时钟，不以真实墙钟断言精确次数。
- **Verify**：单独运行与全套运行结果一致；重复/并发运行稳定；测试结束后不修改用户目录或真实配置。
```

### P0 — TEST-LIFECYCLE-001：持久与动态功能必须跨真实生命周期验收

**状态**：新增候选，建议进入 testing standard；具体产品矩阵继续留在对应 active spec/test。

**跨日证据**：

- 2026-08-17/18：静态调用链完整，MCP commands 在真实 stdio/session 时序中仍缺失或顺序错误。
- 2026-08-21 五个 MCP cache thread：unit/workspace tests 多次全绿，用户在 `./dev.sh` 中仍复现 cache miss；直到区分 cold/write/restart/warm/invalidate 才收敛。
- 2026-08-22 `01a02545-147`：首发 `available_commands_update` 不能证明 discovery 后第二次 stdio update。
- 2026-08-23：compile/clippy 不能替代 Windows Node runtime；live binary 未重启时源码结果也不能代表当前 session。
- 2026-08-24：平台证据 issue 已进一步证明 compile、shared source 和 ignored tests 都不能替代目标 runtime。

**建议规则文本**：

```markdown
### TEST-LIFECYCLE-001

- **Scope**：持久缓存、动态 discovery、异步通知、重连/恢复、外部进程和 transport 行为。
- **Rule**：集成测试必须跨越功能声称支持的真实生命周期。持久缓存至少覆盖 cold fetch → 写盘 → 进程退出 → 新进程 warm hit → invalidate 后回源；动态通知至少覆盖首发与状态变化后的后续事件；transport 行为使用真实 wire/ordering；外部进程功能必须在声称支持的平台执行 runtime 路径。静态共享代码、单进程 unit test、cross-target compile 或 ignored test 不得替代这些证明。
- **Verify**：断言用户可观察结果及必要的请求数、事件顺序、进程边界或目标平台结果；无法运行的矩阵明确标记 blocked/unsupported。
```

### P0 — ARC-CAPABILITY-CLOSURE-001：能力开关必须关闭全部投影与旁路

**状态**：建议强化 `ARC-TOOLS-001`/`ARC-MIDDLEWARE-001` 的 Verify，或新增独立契约；不复制到根路由文件。

**跨日证据**：

- 2026-08-18：commands 在 registry、ACP update、stdio response 时序间出现漂移。
- 2026-08-22 `01a02718-b2f`：关闭 Skills middleware 后 `/skill` 仍由 `SessionManager::build_command_registry` 注册；ToolSearch 依赖关闭后 MCP deferred tools 也随之不可用。
- 2026-08-22 `01a02729-17b`：ToolSearch 静态 core 白名单宣称不存在的能力，后改为 session-local view。
- 2026-08-23 `01a02a2c-94d`：built-in catalog 已过滤，但静态 subagent prompt 仍泄露 disabled agent ID。

**建议契约文本**：

```markdown
### ARC-CAPABILITY-CLOSURE-001

- **Scope**：middleware、provider、built-in capability 与 MetaHarness 开关。
- **Rule**：关闭能力必须在同一 frozen/session-local policy 下同时关闭 direct tools、deferred index/resolver、slash routes、ACP updates、TUI completion、静态 prompt/examples、subagent/workflow 继承与 runtime authorization。只隐藏 catalog/UI 或只移除 middleware 实例不算关闭；依赖其他 middleware 的能力必须显式验证依赖闭包。
- **Verify**：为每个可关闭能力运行 presence/absence 矩阵，覆盖注册、描述、发现、执行和客户端投影；禁止使用静态全局名单代替 session-local view。
```

## 4. 新 Skill 候选

### P1 — `runtime-lifecycle-verifier`

**目标**：为持久/动态功能生成并执行 lifecycle 黑盒矩阵，统一记录启动入口、binary/session provenance、cold/warm/restart/invalidate、首发/第二事件、真实 transport、请求计数和 blocked 平台。

**来源**：2026-08-17～23 至少六天出现“局部绿但真实生命周期未证明”；2026-08-21 MCP cache 最集中。

**与现有 skill 的关系**：

- `diagnosing-bugs` 已正确要求 red-capable feedback loop，不应复制其诊断方法。
- `experiment-driven-research` 已覆盖实验—验证循环，不提供 Peri lifecycle 矩阵。
- `auto-devflow` 已要求最终验证，不定义持久 cache/动态 transport 的具体验收面。

**建议**：先把矩阵固化为项目测试 helper 或脚本；只有当 MCP cache、dynamic commands、plugin reload 等多个功能稳定复用后，再建用户可调用 skill。当前为候选，不建议立即创建。

## 5. 现有 Skill 改进

### P0 — `learn-from-history`

本次执行直接暴露四个问题：

1. `--days 7` 使用 `datetime('now', '-7 days')`，会覆盖今天加此前 7 个日期，实际得到 8 个自然日；文案却称“最近 7 天”。
2. 子 agent prompt 中“使用 general-purpose agent 的写文件能力”被三个 agent 误解为要求递归调用 `Agent`，首次批次因此失败并恢复。
3. 汇总规则仍把稳定 TRAP 默认导向根 `CLAUDE.md`，与本仓库 `DOC-ROOT-001` 和路由规则冲突。
4. 完成守卫不足；当天历史里另一次 `/learn-from-history` 仅输出计划便终止。

**建议改进**：

- 明确 `days` 是自然日期数量还是滚动小时窗口；若是含今天共 N 天，SQL 使用 `-(N-1) days` 的日期下界并补边界测试。
- prompt 改为“你已经是 general-purpose 子 agent，不要调用 Agent；直接使用 Read/Write”。
- 去重基线读取根路由、适用 standards、active specs 和已有 skills；规则按事实源路由到 `docs/standards/`、模块 `CLAUDE.md` 或 active spec，根文件只加路由缺口。
- 完成前强制核对日期数、thread 覆盖数、消息数、日报路径和最终报告路径；阻塞必须列缺失单元。
- 模板中的目录覆盖确认改用 `_index.txt` 对账或 Glob，不要求子 agent 用 `Bash ls`。

### P1 — `ultracode`

现有 skill 已提供 workflow validate/read CLI，并明确 `parallel` thunk 与全 `null` 假成功风险；近期失败说明执行门仍可加强：

- 非平凡 saved workflow 在启动前必须 validate，而非“建议使用”。
- 完成后必须读取 `state.json`/CLI report，核对每阶段 non-null、未截断、verifier verdict 和实际 diff。
- `completed` 只代表 runner 终态，不代表任务验收。
- 写阶段前后记录 branch/status/diff；关键 tracked 文件、ref 或结果目录漂移时停止，不继续覆盖。

证据：2026-08-18 advisor/workflow 卡住；2026-08-22 parse fail、流中断、`null` 阶段；2026-08-23 patch 被回退和结果目录不一致。

### P1 — `handoff`

当前 skill 只要求摘要，没有固定证据字段。建议强制包含：

- 已证实事实 / 假设 / 被用户否定的结论；
- 当前真实启动入口、live binary/session 是否需重启；
- branch/upstream、未提交 diff 与明确排除的用户文件；
- 原始复现命令、尚未覆盖的平台/transport/lifecycle；
- active issue/standard 路径和下一步需要采集的最小证据。

证据：2026-08-21 cache handoff 最终因记录 origin/epoch/TTL/binary 才可继续；2026-08-23 跨重启验收和 Windows skip 若遗漏会重复试错。

### P1 — `langfuse`

现有数据完整性手册已覆盖 parent 链 SQL 与 orphan 症状，但 skill 的默认查询流程仍可强化：

- 先做 host/auth/schema preflight，失败时停止业务归因；
- 明确 API page limit、字段投影和 observation type filter；未请求 input/output 字段时不得判空；
- 增加 trace tree/orphan audit：验证每个 `parentObservationId` 存在、generation/tool/batch 层级与 agent 归属；
- 修复后要求真实 trace 验收，不只依赖 unit mock；区分“代码已修”和“生产进程已重启并产生新数据”。

证据：2026-08-17 多个 Langfuse thread 在查询先决条件、分页、字段投影和真实 parent tree 上反复；具体 orphan 修复已有 active issue，不再重复建规则。

## 6. 已覆盖或已有载体，不建议重复修改

| 模式 | 现有载体 | 结论 |
|---|---|---|
| 平台 CI 中以推测代替运行证据 | `spec/issues/2026-08-24-platform-ci-evidence-discipline.md` | 先执行 issue 验收，成熟后再进 standard |
| 运行时 bug 先建反馈环 | `diagnosing-bugs` / `diagnose` | skill 已很强，近期问题是未触发或未遵循 |
| 当前工具能力以 session-local view 为准 | `ARC-TOOLS-001` | 已覆盖并已有修复，不新增同义规则 |
| Advisor 必须收到 Decision Packet | `advisor-consultation` | 已完整覆盖；失败来自未使用该 skill |
| 博客不得写无测量性能承诺 | `blog-writer` | 已覆盖并要求独立事实审查；属于执行偏差 |
| 禁止跳过 Git hooks | 系统 Git safety + 现有约束 | 已覆盖，不新增重复条款 |
| 从 `origin/main` 建 feature branch 不绑定 upstream | `GIT-BRANCH-001` | 已覆盖 |
| Windows PTC production E2E 缺口 | `spec/issues/2026-08-23-windows-ptc-production-e2e-disabled.md` | 保持具体 active issue，不泛化复制 |
| MCP Agent origin identity 碰撞 | MCP Agents 具体实现风险 | 应进对应 active spec/issue，不做通用 skill |
| TUI 视觉 padding 污染语义复制 | 单次明确回归 | 可在下一次同类改动时补 `TUI-TEXT` Verify；当前不足以单独建 skill |

## 7. 成功模式

1. **对抗审查后再独立验证**：MCP cache、PTC、工具审计中，reviewer 能发现 TOCTOU、identity、权限和跨平台问题；verifier 又能消除高危误报。价值来自反证和可执行检查，不来自 agent 数量。
2. **运行时事实源收敛**：ToolSearch 改用 session-local tool view；built-in subagent catalog 与 runtime 共用 frozen policy；协议角色在事件产生端确定，wire 层只序列化。
3. **安全默认 fail closed**：cache 在 digest/frontmatter 验证后才落盘；认证上下文隔离；PTC 错误使用 allowlist/canary；远程 MCP Agent 危险本地字段被清除并复用父权限交集。
4. **视觉与语义分层**：TUI 最终只移除可识别 visual padding，不用 `trim_end()` 污染用户源码；同类 UI 状态也逐步区分 fetch、stored 与 hit。
5. **确定性实验替代放宽断言**：固定测试时钟、临时 HOME、真实 SQLite wire、LSP 同步屏障和重复运行都比修改生产逻辑规避 flake 更可靠。
6. **精确提交范围**：多次排除 `peri-cool`、`.mcp.json`、用户脚本和并行改动；高风险 rebase/squash 前保留 backup，并在用户明确要求后才提交或推送。
7. **文档按事实源路由**：2026-08-22 后根文档逐步恢复路由职责，稳定规则进入 standards，active changes 留在 spec/issues。这应继续保持。

## 8. 统计摘要

- **规则/验收候选**：4 个，均为 P0
- **新 Skill 候选**：1 个，P1，建议先实现为测试 helper/script
- **现有 Skill 改进**：4 个，其中 `learn-from-history` 为 P0
- **已有覆盖或 active issue**：9 类，不建议重复写入
- **外部项目发现**：0；`example/` 路径均属于当前仓库

## 9. 建议应用顺序

1. 先修 `learn-from-history` 自身的日期边界、prompt 歧义、文档路由和完成守卫。
2. 将 `TEST-EVIDENCE-001` 与 `TEST-HERMETIC-001` 合并评审后写入 testing standard。
3. 为 MCP cache/stdio dynamic update 先落地一条真实 lifecycle 测试，再决定 `TEST-LIFECYCLE-001` 的最终措辞。
4. 强化 capability disable 的契约测试矩阵；优先扩展现有 `ARC-TOOLS-001`/middleware Verify，避免再造平行事实源。
5. 按实际使用频率再决定是否创建 `runtime-lifecycle-verifier` skill。

本报告生成阶段只记录建议；用户确认后的应用结果见下节。未提交任何变更。

## 10. 应用结果

用户选择“应用全部现有项”后，已完成：

- `docs/standards/testing.md`：新增 `TEST-EVIDENCE-001`、`TEST-HERMETIC-001`、`TEST-LIFECYCLE-001`。
- `docs/standards/architecture-contracts.md`：新增 `ARC-CAPABILITY-CLOSURE-001`，与 `ARC-TOOLS-001` 的 session-local view 契约保持分工。
- `.claude/skills/learn-from-history/`：修正含今天在内的自然日期边界、非递归子 agent prompt、规则事实源路由、多日期分组和完成对账；新增隔离 SQLite 的 Python 测试。
- `.claude/skills/langfuse/`：增加 host/auth/schema/分页/字段投影 preflight、真实 trace 验收门和 metadata-only observation tree/orphan audit。
- 用户级 `handoff` skill：增加事实/假设/否决项、runtime provenance、仓库漂移、验证矩阵和后台任务状态字段。

未执行项：

- 未创建候选 `runtime-lifecycle-verifier`。
- builtin `ultracode` 没有可编辑磁盘源；为避免项目级同名 override 遮蔽完整 builtin 行为，本次标记为 **blocked**，未创建 override。
- 未触碰任务开始前已有的 `peri-cool` 工作区改动，未 commit。

验证证据：

- `python3 -m unittest discover -s ".claude/skills/learn-from-history/tests" -p "test_*.py" -v`：2/2 passed。
- `python3 -m py_compile ...`：passed。
- 首次 `bun test ".claude/skills/langfuse/scripts/lib.test.ts"` 被 Bun 解释为 filter，实际执行 0 tests 并 exit 1，不计为通过；修正为 `bun test "./.claude/skills/langfuse/scripts/lib.test.ts"` 后 8/8 passed。
- `bun "./.claude/skills/langfuse/scripts/trace-tree.ts" --help` 及两个 Python CLI `--help`：passed。
- `git diff --check`：passed。
