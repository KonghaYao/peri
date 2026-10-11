# P0：移除历史会话加载中的旧运行状态恢复，保留上下文与正常续聊

> 2026-10-07 后续裁决：本 issue 中持久执行恢复/WorkState 优化部分被[完整剥离计划](2026-10-07-remove-execution-recovery-plan.md)取代；既有历史观察与验证记录保留，不作为保留执行账本的要求。

- 优先级：**P0（用户定级）**；不据此推定影响人数、损失或复现频率。
- 状态：**Active / 快速修复已实施、关键回归通过，邻近测试失败与剩余迁移待闭环**。不代表整个 P0 已闭环。
- 裁决来源：2026-10-06 用户明确纠正“会话恢复”语义，并要求删除新增的运行状态恢复。
- 建档阶段仅新增本文件；后续快速修复覆盖 TUI 历史/后台投影、SDK load 默认激活与 ACP observer 历史扫描提示，未改用户数据库、未运行 E2E；用户随后授权提交本轮修复。

## 1. 目标与术语

**会话恢复 = 加载历史消息、恢复冻结上下文、继续对话；不等于复活旧 execution。**

删除“加载已有会话即默认恢复旧执行/投影旧运行态”的行为。历史事实可展示，但不得单凭旧 tool、subagent、work 的非终态记录推断当前有执行，驱动 loading、自动续跑或绑架正常新输入。当前活跃必须有可核实的当前执行证据；snapshot 是事实投影，不天然是 live 证据。

这不是禁用 `session/load`、删除历史、只读历史模式或停止 spinner 的局部补丁。实施必须拆开历史展示、持久事实、当前执行与新输入准入的职责，并删除过时恢复分支，不新增兼容双轨。

### 保留 / 删除边界

| 保留 | 删除或解除耦合 |
| --- | --- |
| 历史查看、会话切换、冻结上下文、手动续聊 | load/replay 自动激活旧执行 |
| 历史工具输入/结果、后台任务事实与未知结果展示 | 历史 started 或非 terminal 直接转为当前 running/active |
| 可靠交付、持久绑定、owner 对账、去重与副作用屏障 | 旧阶段恢复失败无差别阻塞正常新输入 |
| 当前真实任务的事件、进度、终态、取消及合法激活 | 无当前执行却持续 loading、伪造开始时间或恢复进度 |

不一概删除可靠交付或 owner 管理。当前执行所需阶段投影也不能因名称包含 `recover` 就机械删除；应根据触发来源、执行身份与副作用边界判断。

## 2. 证据与边界

### 用户观察

用户截图及反馈：缓冲区消息无法发送；历史恢复后出现 loading，subagent 卡住，会话无法正常使用。用户认为“实际没有进行任何操作”；这是用户观察，尚未通过全链路执行计数证明，不把它扩展为所有旧任务都未执行。

### 当前源码核对

| 入口 / 符号 | 已观察行为 | 实施时需核实的边界 |
| --- | --- | --- |
| `peri-tui/src/kit/acp_events/tool.rs` / `handle_replay_tool_started` | 历史工具卡设 `is_running = true`，折叠状态为 Running；结束重放另行更新 | 缺失 ended 时展示历史未确定结果，不宣称当前工具在运行 |
| `peri-tui/src/kit/acp_events/system.rs` / `apply_bg_task_snapshot` | 以 `!terminal` 设 `is_active`，非终态调用 `seed_live_from_started`，并纳入 live reconcile | 快照状态、当前执行证据及 UI activity 分开；不能只改一个字段 |
| `npm-packages/@peri-sdk/src/agent/session.ts` / 已有会话 load 分支 | 加载后调用 `ensureProcessing("recovery")` | 加载历史与执行激活解耦；该 SDK 入口尚不能直接认定为 TUI 事故根因 |
| `peri-agent/src/agent/stages/work_receive.rs` / `recover_work` 调用 | 从持久 work 投影恢复 stage；ReasonReady / ActReady 被判为 live，其他阶段可能要求对账或报错 | 区分当前准入批次投影与旧执行复活，追踪 SDK → ACP → Agent 的实际调用链 |
| `peri-agent/src/agent/stages/work_recovery.rs` / `RecoveredStage` | 保留模型请求不确定、invocation 对账、Blocked 等状态 | 不删除未知副作用证据，不将未完成改写为成功或未执行 |

以上是读取当时的共享工作区快照；其他线程正在修改后台 observer 与状态链路，实施前重新核对，不把本表当最终调用图。

### 日志与既有调查

- 原日志：`.tmp/agent-tui.2026-10-06`。2026-10-06 **05:03:14.869812 UTC、05:03:18.702678 UTC** 的 `session/prompt` 被 `domainWorkBlocked` 拒绝，TUI keepgoing 消费者收到失败；本轮已直接核对。
- **05:07:21.555647 UTC** 日志记录 internal execution failure。本轮日志不能单独证明其具体底层原因。
- `spec/issues/2026-10-06-tui-execution-failures.md` 的既有只读持久化调查记录 child 恢复身份/授权不可用，以及 Read invocation 的 `StaleRevision`。本轮未重新查询数据库；不将调查推断提升为新的独立复现。
- 按既有交付记录，`cdaf8914` 已修复重复 invocation 准入写入；这不构成本 issue 的历史加载、活跃证据或新输入准入闭环。本 issue 不重复修该并发问题。

**根因方向（待全链路验证）**：历史事实被复用为当前执行状态，同时加载/准入与旧 work 恢复绑定。上述源码支持这一方向，但尚不足以证明每条 loading 与缓冲区失败都由同一入口引起。

## 3. 期望与实际

- 期望：load 只恢复消息与冻结上下文；没有当前执行时不 loading。历史未确定结果有准确静态状态。用户新输入获得真实执行准入或明确安全拒绝。
- 实际证据：replay/snapshot 可直接生成 Running/active；已有会话 SDK load 会请求 recovery 激活；旧恢复相关 blocked 记录存在，继续请求遭 `domainWorkBlocked` 拒绝。
- 验收禁止：仅重置 spinner、吞掉错误、改成通用“内部错误”，或只验证 history 可见却不验证手动续聊。

## 4. 分层实施范围与依赖

1. **契约与领域**：明确 history projection、durable fact、current execution evidence、input admission 的职责与身份。列出所有 load/replay/recovery 激活入口，区分加载、显式新输入与当前任务通知。
2. **TUI**：隔离 replay 与 live 更新；历史缺工具结果、后台非终态、旧任务快照不能创建 live 执行。会话反复切换不泄漏前一会话 activity。缓冲区消费在拒绝后保持内容及可操作状态，不永久等待不存在的执行结束。
3. **SDK / ACP**：移除已有会话加载默认复活旧执行的触发，审计 load、prompt、queue、continuation、后台 observer 的耦合。保留合法消息交付与当前执行通知；不能把通知订阅本身当当前活跃证明。
4. **Agent / work**：使旧恢复失败不无差别绑架新 work；保留当前批次的身份、预算、授权和副作用校验。旧 work 不自动重新发模型请求或工具；真实当前执行仍可运行、取消和完成。
5. **迁移与文档**：确定旧记录分类及安全处理契约后实现并验收。同步受影响设计、标准、模块路由与 code-index；本轮只记录此义务，不修改它们。

**设计依赖**：`docs/design/session-async-tasks.md` 区分持久绑定与 owner 事实，并包含执行恢复目标；`docs/design/rcra-message-activation.md` 包含未完成检查点/阶段续接作为可运行来源。本次用户裁决改变“load 默认复活旧执行/投影旧运行态”的目标行为，不能声称既有设计已完全符合。实施须同步受影响条款，并与 `docs/standards/architecture-contracts.md` 对齐；不借机删除可靠投递、owner 对账、去重或当前任务取消。与现有 RCRA active issue、后台 observer 修复协调写集和契约，不覆盖其他线程改动。

## 5. 安全迁移待决点

- 旧 work / child / invocation 的生命周期与未知副作用如何分类、隔离、展示？不得把未知当“没执行”，不得清库、清队列、强制完成、伪造终态或自动重发工具来解卡。
- 新输入与旧未知副作用何时存在真实冲突？原则上不因旧恢复失败全局封锁；必要安全拒绝须限定冲突范围，并说明旧记录身份、冲突类别、必要证据和确实可用的下一步操作。尚无安全操作路径时明确告知，而不是承诺不存在的按钮。
- 冻结上下文如何在不复活旧执行的前提下保留？旧持久投递责任如何继续对账而不触发默认续跑？新 work 与旧 work 的隔离边界需由实施给出并验证。
- 如何证明 current live：须说明证据的来源、身份、有效期及失效方式；pid、非 terminal、旧时间戳或快照存在均不足以独立证明活跃。

迁移策略尚未批准或实现；安全待决点不能成为继续无执行 loading 的理由。

## 6. 用户可观察验收矩阵

| 场景 | 必须看到的结果 / 回归要求 |
| --- | --- |
| 历史只有 tool started、缺 result | 历史可查看，未确定结果静态可辨；不转圈、不自动调用工具；可手动续聊或得到具体安全拒绝 |
| bg 无 live 实例，旧状态分别为 running、lost、reconciling、delivery_pending、未知值/缺失 | 逐项参数化验证；均不单凭非终态变成 live running，保留原始事实与投递义务 |
| 进程重启后加载旧会话 | 消息与冻结上下文保留；不恢复旧 execution、不自动发模型/工具请求；无当前执行则无 loading |
| 反复切换旧会话 / 重复 load | 历史稳定、无 activity 泄漏或重复激活；不会重复交付逻辑消息 |
| 手动输入及既有缓冲区消息 | 明确发送/准入结果，不被旧 child 恢复失败无差别阻塞；失败时内容可见且可操作、不伪装正在执行 |
| 本次真实活跃的主任务、tool、subagent | 真实事件仍驱动 loading/进度；终态及时退出；合法取消、后台结果与可靠交付正常工作 |
| 旧未知副作用确实与新输入冲突 | 无自动重试副作用；明确冲突来源和可行操作；不泛化 internal error，不无执行 loading |
| SDK load 与 TUI 实际 load 路径 | 分别验证加载历史本身不默认执行，同时保留手动续聊；额外追踪 sidecar/ACP，不能以 SDK 单测替代 TUI 行为验收 |

每项同时检查 UI 与执行证据：历史加载是否产生模型请求、工具 invocation、新执行准入；仅截图无 spinner 不足以通过。真实当前执行用独立对照验证，不为让负例通过而关闭所有运行事件。

## 7. 目标验证与完成门槛

遵循 `docs/standards/testing.md`，新增/修改测试覆盖上表的用户行为，再补阶段、准入和未知副作用契约；Rust 过滤测试须确认非零测试数。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib
(cd npm-packages/@peri-sdk && bun run test && bun run typecheck)
git diff --check
```

最终 TUI 完整行为验收须先读 `e2e/CLAUDE.md`，协调控制面与隔离测试数据，按影响范围执行 E2E；发布级闭环目标为 `npm --prefix e2e run e2e:release`。不在其他线程独占 E2E 时并发运行或清理其环境。本轮未执行以上全量套件、typecheck 或 E2E；已执行的定向测试见 §8。

- [ ] 实施前补全真实 TUI → sidecar / SDK → ACP → Agent 激活链与当前证据契约。
- [ ] 删除默认旧执行恢复耦合，保留完整历史与正常续聊；同步受影响设计/标准。
- [ ] 安全迁移待决点有明确决策与验证，不绕过未知副作用约束。
- [ ] 验收矩阵及真实当前执行对照通过，报告实际测试结果与未覆盖范围。
- [ ] 协调相关 active issue 后更新稳定事实源，再按 `DOC-HISTORY-001` 关闭本 issue。

## 8. 快速修复记录

- TUI 历史工具卡不恢复 running；缺结果静态提示历史记录不完整，后续历史结果仍按 tool ID 补全。复用现有 error 展示，未引入新的历史结果状态类型。
- 后台 snapshot 保留原始 task 事实；无当前 started 证据的非终态为 Unobserved，不创建当前活跃行、不伪造开始时间。真实 started 可以激活，lost 等增量更新也撤销活跃观察，快照不能再次复活失去观察的执行；终态与已有输出保留。SDK 同步投影为 `unobserved:<原状态>`，running/waiting 可以保留已有真实 start 证据；lost、reconciling、delivery_pending、未知状态或终态均撤销证据，后续快照不能单独重新授予活动。
- UserInputDelivered 不启动 loading；当前 RunStarted/实时执行事件仍驱动 loading。后台 SubagentStarted 不把空闲主回合拉回 loading。
- SDK 删除已有会话 load 后默认的 `ensureProcessing("recovery")`，保留手动新输入和当前 work notification 的准入。
- ACP observer 登记时记录持久投递序号分界；历史遗留 work 单独不发 `session/work/available`，新的未处理 Required delivery 仍按既有规则扫描通知，显式 cron/continuation 发布不受抑制。旧交付义务保留，不改准入、不绕过未知副作用；新通知后的旧 work 候选隔离仍未闭环。
- TUI `kit::acp_events::acp_events_test`：141 passed / 0 failed / 4 ignored；`shell_detail`：10 passed / 0 failed。SDK 指定 task snapshot、task projection recovery、session docs wire、session view、session view incremental、session start cleanup、execution session 七个测试文件：59 passed / 0 failed，352 assertions。均为定向测试，不是完整 E2E。
- ACP `host::continuation::tests`：6 passed / 0 failed；`host::requests::tests::user_input_tests`：7 passed / 5 failed。新增历史抑制/新投递通知与既有无 MQ hint 扫描回归通过。五个失败为 takeback、pause、wire-control、stdio、work-notification，涉及 queued/dispatching、MQ 提示及空 ticket 断言；临时恢复原 observer 扫描行为（传入 `None`）重跑仍同样 7 passed / 5 failed，随后恢复修复。此对照只隔离 observer 分界的影响，不证明其他并行 WIP 或整个仓库基线通过；未在本轮修复这些失败。

**仍未闭环**：旧 work/child 恢复失败与新 work 的安全隔离、未知副作用迁移、真实重启/反复切换验收尚未实现或验证。未重新检查用户数据库，未证明 SDK load 是 TUI 事故唯一根因；不得把本轮定向测试通过当作整个 P0 已修复。
