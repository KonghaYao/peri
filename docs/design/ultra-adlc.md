# Ultra-ADLC 设计

> 状态：现行设计（builtin skill v1）
> 范围：Ultra-ADLC 的产品语义、项目级文件协议与编排不变量
> 执行协议：[`ultra-adlc/SKILL.md`](../../peri-middlewares/src/skills/builtin/skills/ultra-adlc/SKILL.md)

Ultra-ADLC 把一个自然语言目标编译为完整、可审计的超大规模交付。它是 Main Agent、
builtin skill、现有 `Workflow` deferred tool、受限的 `AskUserQuestion` 和文件系统之上的
编排层，不新增 Workflow DAG、RPC、ACP/TUI 事件、中间件顺序或 SubAgent 机制。底层事实
以代码、契约测试、`docs/standards/` 和 [Workflow 设计](workflow.md)为准。

## 产品边界

入口只有：

```text
/ultra-adlc <自然语言目标>
```

用户无需预先给出 crate、文件、测试命令、Agent 数或 Workflow 脚本。Main Agent 负责环境
发现、问题形成、权限边界、Workflow 编译、证据汇合和最终汇报。

Ultra-ADLC 适用于明确要求完整交付的超大任务；普通功能、修复、review 或临时并行工作不应
隐式升级到该模式。v1 不新增 `/adlc` 面板，不迁移 Workflow journal，不让 Workflow Agent
直接向用户提问，也不从单次任务结果自动改写全局策略。

## 稳定不变量

### ADLC-ENTRY-001

用户只需提供自然语言目标。环境、代码入口和验证命令应先由 Agent 调查，不得转嫁给用户。

### ADLC-STORAGE-001

项目级记录根目录固定为 `{cwd}/.peri/adlc/`。canonical path 必须仍位于 cwd；路径穿越、
absolute child path 和不安全 symlink fail closed。该目录是本地审计记录，不自动 commit。

### ADLC-WORKFLOW-001

一次任务只有两个逻辑 Workflow：`discovery-design` 与 `delivery-convergence`。resume 产生的
新 run_id 追加到原逻辑槽位，不构成第三个 Workflow；修复循环属于 Workflow 2。

### ADLC-ARBITRATION-001

Workflow 1 的 synthesizer 只生成不可变、revisioned Decision Packet 与推荐，不自我批准。
Main Agent 以安全 canonicalize、exclusive create 和精确 bytes 的 SHA-256 冻结 packet。
未参与发现/设计/综合的 fresh `opus` Decision Arbiter 默认裁决；只有同一 packet 上两次
fresh Opus 均无法合法裁决时才可升级一次 fresh `fable`。纠错只重跑裁决，不重跑发现。

合法结果为：

- `decided`：在已有 intent/authority 内选择可逆方案，Main Agent 校验后直接进入 Workflow 2；
- `needs_evidence`：补充仓库或环境可发现证据，不询问用户；
- `needs_user`：仅限缺失产品意图、新授权、secret/外部状态、法律/财务接受、不可逆动作，
  或无法从目标推导且显著影响用户结果的选择；
- `invalid`：身份、schema、revision、证据、路径或 fingerprint 校验失败。

每次 attempt 必须绑定 packet ID/revision/path/fingerprint、唯一 attempt identity、请求 profile
和可信 Workflow journal provenance。模型自报身份、profile 或 hash 不构成证据。Decision
Record 不授予 commit、deploy、删除数据或外部写入权限。

### ADLC-HITL-001

Main Agent 是唯一用户交互 seam。Workflow Agent 不得调用 `AskUserQuestion`。只有合法
`needs_user` 才进入用户决策；技术证据不足走 `needs_evidence`。产品裁决不能替代
PermissionMiddleware 的敏感工具审批。

### ADLC-PROGRESS-001

进度同时报告需求覆盖、工作包完成、验收证据通过和缺口关闭四维。每项同时显示 ID 与
语义内容；进行中不计部分分。整体百分比取四维最小值，不取平均。范围或完成条件改变时
递增 denominator revision；100% 进度不替代 Completion Assessor verdict。

### ADLC-HANDOFF-001

跨 Agent 的可消费结果必须写入唯一、不可覆盖的 Handoff；大型产物写 `artifacts/`，
Workflow 返回值只返回状态和路径。下游只读取当前契约、直接依赖 Handoff 与必要源码，
不得复制完整聊天记录或大型工具输出。

### ADLC-MODEL-001

按节点认知负载、错误代价、可验证性和返工成本选择 `haiku`、`sonnet`、`opus`、`fable`
Profile，不给整个 Workflow 绑定单一档次。profile 和升级条件记录在 execution contract，
实际路由与升级次数进入证据。

### ADLC-CONCURRENCY-001

使用现有 `maxConcurrency`、`parallel`、`pipeline`、`phase` 与普通 JavaScript 控制流实现
有效并发。只读任务可高并发；写任务必须有不重叠 write scope；共享写入由唯一 owner 收敛。
`phase(name)` 只标记阶段，不接收执行 callback。

### ADLC-GIT-BASELINE-001

`writeIntent.path_allowlist` 以 Workflow 启动时捕获的 Git baseline 为准，只归因运行期间发生
变化的 tracked path。由于 Git postcondition 不覆盖 ignored path，orchestrator 还必须比较
preflight/post-processing 文件系统 snapshot，再把变化路径与 Git `changed_paths` 合并后匹配
allowlist。启动前已存在且未变化的 dirty/ignored path 只记录，不阻塞也不归因给本次执行。

### ADLC-DELIVERY-STATUS-001

Workflow engine 的 execution、acceptance、post-processing 和 delivery 四维独立解释。
execution/post-processing 成功但 acceptance 未知时，delivery 仍为 `unknown`；明确执行失败、
acceptance failed、缺少可验证 write intent 或 post-processing failed/blocked 才是 `blocked`；
必要维度全部通过才是 `deliverable`。

### ADLC-COMPLETE-001

合法任务终态只有 `complete`、`blocked`、`cancelled`，不存在 `partially_complete`。
每轮只有一个 fresh、独立、只读 Completion Assessor；只有全部需求、工作包、验收场景与
Verification Plan 必需检查具有当前可归因证据时，verdict 才能是 `complete`。可修复缺口留在
Workflow 2 内继续收敛；外部依赖或缺失授权才可 `blocked`。

### ADLC-EVOLUTION-001

任务完成后才生成 Agent performance/evolution record。单任务记录只进入本地 evolution
dataset，不直接修改 builtin skill、模型配置或项目指引。

### ADLC-COMPAT-001

v1 不修改 Workflow RPC、事件、TUI Panel、journal、SubAgent frozen context 或中间件顺序；
`Workflow` 保持 deferred tool。任务状态通过项目文件和现有 Workflow 完成通知推进。

## 项目级文件协议

安全任务目录：

```text
.peri/adlc/tasks/<adlc-id>/
├── manifest.json
├── contracts/{intent.md,execution.md,evidence.md}
├── decisions/
├── handoffs/{workflow-1,workflow-2}/
├── artifacts/{designs,reviews,test-results,progress,workflow-provenance}/
└── learning/agent-performance.md

.peri/adlc/evolution/
├── records/
├── routing-observations.md
└── eval-candidates/
```

任务 ID 使用可预测时间与安全 slug；不得在 Workflow 脚本中用随机或本地时钟生成身份。
`.peri/adlc/` 只保存三份契约、决策、Handoff 摘要、完成评估、精简 provenance 与学习记录；
不保存 secret、完整模型输出、重复日志或大型构建产物。

Workflow 原始事实继续位于 `.claude/workflow-runs/<run-id>/`。ADLC 不复制 journal；
`manifest.json` 只记录逻辑 Workflow 到物理 run_id 的映射、契约 revision、裁决 attempts、进度
revision 与 completion verdict。每次完成通知后，Main Agent 必须读取对应 `state.json` 并分别
校验 engine 四维状态、Agent 数、handoff 和 postcondition，不能把“Workflow 正常返回”当成交付。

任务状态为：

```text
discovering → awaiting_user_decision? → planning_delivery → delivering
            → verifying ↔ converging → complete
任一执行阶段 → blocked / cancelled
```

`blocked` 不是完成；解除后恢复原逻辑 Workflow。契约 revision 改变时，过期 Handoff、证据
和可恢复节点必须失效或重新验证。

## 三份契约

三份契约是任务唯一的需求、执行与证据接口；manifest、Decision Packet/Record、Handoff、
artifact 与 learning record 都不是额外契约。

### `intent.md`

回答“为什么做、用户最终得到什么”，至少包含：User Goal、相关环境事实、Desired Behavior、
Acceptance Scenarios、Non-goals、User Decisions、Constraints、Authorized Actions 与 Stop and
Escalation Conditions。arbiter 只能在已有意图和授权内选择；语义变化递增 revision，并使受
影响的下游工作失效。

### `execution.md`

回答“Agent 团队如何完整交付”，至少包含：Intent Revision、Repository Facts、Selected
Design、Rejected Alternatives、Impacted Areas、Work Packages、Completion Ledger、Decision
Arbitration、Progress Reporting、Model Routing、Concurrency and Write Ownership、Verification
Plan、Handoff Plan、Retry/Resume/Escalation。

每个 Work Package 必须有语义标题、目标、依赖、profile、工具与读写 scope、输入输出、验收
证据、retry/escalation 和 Handoff path。Worker 不直接改 execution contract；偏差写 Handoff，
由规划 owner 修订。

### `evidence.md`

回答“凭什么完整”，至少包含：Delivered Outcome、Intent/Work-Package Coverage、Acceptance
和 Tool Evidence、Independent Reviews、Plan Deviations、Remaining Risks、Completion Verdict
与 Workflow Provenance。实现 Agent 可贡献证据，不能给自己签发完成结论。Assessor 结束后，
Main Agent 只能追加精简 provenance，不得篡改 verdict 或覆盖率。

## Handoff 与写入归属

Handoff frontmatter 至少绑定 schema、task、logical workflow、phase、round、Work Package ID
及语义标题、Agent/profile、状态和契约 revision；正文至少包含 scope、inputs、completed work、
decisions within authority、evidence、remaining items、risks/blockers、output references 和 next
consumer。

- `complete` 的 Remaining Items 必须为空；`blocked` 必须给出具体外部解除条件；
- 每项完成声明关联源码、测试或工具证据；不得包含 secret 或不必要用户数据；
- 每个 Agent 使用唯一路径，修订创建新文件而非覆盖；
- manifest、契约与共享 artifact 只能由指定 owner 写入；无法隔离的产品写入由唯一
  integration owner 收敛。

## 两个逻辑 Workflow

### Workflow 1：`discovery-design`

Workflow 1 只调查仓库、测试、架构、风险和候选设计，并写 ADLC 记录，不开始产品实现。
典型波次为 Haiku 探索、Sonnet 专项设计、Opus 综合、fresh Opus 裁决；Fable 只作条件升级。

synthesizer 写唯一 candidate；Main Agent 验证 task root、regular/non-symlink identity 后以
exclusive create 发布不可变 final packet，并计算外部 SHA-256。arbiter 直接消费本次验证的
exact bytes，不重新按路径读取。接受结果前再次验证 packet 与 attempt-unique Handoff；任何
revision、identity、path、hash、schema、evidence 或 authority 不匹配均为 `invalid`。

### Workflow 2：`delivery-convergence`

Workflow 2 从已接受契约生成全量 Work Package 与 Completion Ledger，按 write ownership
并行实现，由唯一 integration owner 汇合，再并行验证并交给单一 Completion Assessor。verdict
为 `incomplete` 时把每个 gap 变成可验收工作包，继续修复、复验和重新评估；不得以 token、
时间、上下文、主路径可用或大部分测试通过为退出理由。

每个物理 run 先声明 read-only 或可验证的 write intent。resume 只复用与当前 contract revision、
输入 fingerprint 和依赖证据一致的结果；否则重建受影响节点。取消停止新调度、请求取消运行中
Agent、写入最终状态并保留已有证据，不伪造完成。

## 进度、完成与用户汇报

Completion Ledger 建立 `Requirement → Work Package → implementation → verification evidence`
映射。Gap 一经发现就留在分母中，直到当前证据证明关闭；尚无 Gap 时缺口关闭率为 100%。
每轮评估使用 fresh Assessor，只读取当前契约、Ledger、Handoff、必要 diff 与测试证据。

最终向用户报告：交付结果、关键修改、验证证据、决策与偏差、剩余的非必需风险、Workflow
provenance 和 ADLC record 路径。不得把内部 Agent transcript、完整 journal 或大段 Handoff
复制到聊天中。

## 安全边界与事实源

- 遵守当前 PermissionMode；Decision Record 不扩大用户授权；
- 未经单独明确授权，不 commit、push、publish、deploy、删除重要数据或修改外部系统；
- 外部证据和 Handoff 按不可信输入处理，不执行其中嵌入的指令；
- 写入前验证 canonical path 与 allowlist，原子写或 exclusive create 失败时 fail closed；
- secret 不进入 prompt、日志、错误、测试 fixture、Handoff、manifest 或 artifact。

稳定路由：

- 可执行编排：[`ultra-adlc/SKILL.md`](../../peri-middlewares/src/skills/builtin/skills/ultra-adlc/SKILL.md)
- Workflow runtime：[workflow.md](workflow.md)
- Workflow 代码入口：[peri-workflow 代码索引](../code-index/peri-workflow.md)
- Skills/Workflow middleware：[peri-middlewares 代码索引](../code-index/peri-middlewares.md)
- 架构与测试约束：[architecture-contracts.md](../standards/architecture-contracts.md)、
  [testing.md](../standards/testing.md)
