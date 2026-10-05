# RCRA 一等公民会话消息与激活实施

状态：**重构中**——第 1、2 步已完成并验证；第 3–5 步未实施。生产运行时发布门槛均未完成，不得宣称整体已解决。

## P0 验证计划

本次架构调整按 P0 风险审查。此前六个反例关闭只属于局部静态证据，不是整体可实施性或运行时通过。

- 研究问题：每会话一等公民隔离、可靠接纳、持久处理责任和 SDK 唯一执行者能否同时成立；崩溃、乱序与控制竞态中是否存在责任空档。
- 成功标准：阻断级反例有明确裁决和闭环，关键时序经可复现模型探针验证并由独立评审挑战；实现依赖、假设及尚无运行时证据的发布门槛逐项记录。
- 控制变量：只修改本任务权威/active issue；不改已有脏代码，不以旧实现否定新目标，不把模型探针视为生产系统证明。
- 第一轮：四路只读对抗领域/授权、存储/责任、控制/竞态、跨层/迁移；提取最小反例。
- 第二轮：对成立反例修正文档，并运行有限状态/故障切点模型；独立验证模型是否偷加前提或漏掉故障窗口。
- 结论门槛：文档一致性、有限模型验证、真实端到端分别评级；未实施体系不能报告运行时 PASS。涉及未批准产品语义的取舍先裁决，不静默降级。

## 权威与变更范围

- [RCRA 消息、任务归属与激活](../../docs/design/rcra-message-activation.md) 为消息/处理义务/激活单一权威。
- [Session 异步任务](../../docs/design/session-async-tasks.md) 负责每会话任务目录、执行 owner、恢复与关闭。
- 主/子/嵌套/Workflow Agent 完全同构；父会话拥有委托关系，不拥有子内部 Task/MQ。
- 替代旧 root fallback、易失队列接纳冒充可靠送达、Transcript 存在冒充处理完成、按任务种类特判 continuation 的目标。
- 显式 Stop 暂停该会话自动激活（子 Agent 的停止走关闭协议）；Independent 子会话不被隐式暂停。自然完成后允许 EnsureProcessing 再激活。

## 重构顺序

1. [x] 会话级 MQ 同构：子会话收件登记、唤醒绑定与独立任务目录；嵌套委托登记到直接父，消除跳层。
2. [x] 投递归属：入站绑定发起者、冷恢复重建；删除 root fallback，保留并扩展已暂存的 initiator 守卫。
3. MQ 消费语义：类型属性与消费矩阵进 Receive；收敛 continuation/idle/async_router 特例，唤醒降级为通知。
4. 关闭与控制：关闭子会话级联终止 bg shell、资源终止并结算后返回消息；Stop/Pause/Resume/Close 幂等与 attempt 精确定位。
5. 可靠连接下的幂等与持久：事件身份去重、required 不降级、mutation Unknown 冻结、投递义务可恢复；随后按 §7 矩阵与发布门槛验收。

### 第 1 步实施（2026-10-05）

- 子会话构造自身 `SubagentHost`，按稳定 child session ID 登记 Inbox 与 TaskManager；MCP 池保留会话运行时目录至显式注销，同进程 resume 复用原 MQ/任务目录，不依赖原父 runtime。
- spawn/resume 不再给子执行注入 root task owner；Workflow Agent 同样先登记自己的会话、MQ 与任务目录，再调用 MCP 工具。执行树 root 只保留原有持久化/委托关联用途。
- 继承的 Agent 工具通过 `SubagentChainAssembler::bind_tools` 重新绑定直接父会话、AgentId、取消令牌与工具授权上限；嵌套后台委托登记在直接父目录，回执进入直接父 MQ，不跳到祖父。
- 子 loop 接入已有 bounded idle、registry activity、deadline 与未结算交接探针；只是补齐会话装配，不修改 Receive 分级规则或另设任务类型唤醒路径。
- 回归入口：`session::subagent::v2_bridge::session_wait_tests`、`subagent::tool::tests::session_isolation_test`、`mcp::client::output_store::tests::bound_child_mcp_task_receipt_uses_its_own_catalog_without_root_override`、`assembly::tests::workflow::mcp_owner`。覆盖隔离收件、自然 idle 后消费终态、直接父登记/回执、父 runtime 替换后的 MQ/目录复用、嵌套工具授权不扩张及真实 MCP wire scope。
- 验证：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares -p peri-agent --lib`：Agent 899 passed；Middlewares 1560 passed / 2 ignored。相关三 crate doc tests、ACP 编译、相关三 crate all-targets clippy（`-D warnings`）、fmt、层依赖与 diff 检查通过。本次修改源码/测试均不超过 1000 行；全库大小扫描仍有 14 个未触碰的存量超限测试文件。
- 边界：没有实现进程重启后的目录/Inbox 恢复、退出后的统一自动激活、required 接纳/持久处理义务或关闭协议；冷恢复 root fallback 留给第 2 步，消费/调度收敛留给第 3 步，关闭与持久保证分别留给第 4、5 步。不得把内存目录保留或一次唤醒等同于可靠接纳或 P0 发布验收。

### 第 2 步实施（2026-10-05）

- 删除 `ToolContext.mcp_task_owner_session_id` 及 dispatch override；在线 MCP 调用在发送前按可信当前会话准入，scope、任务目录和投递归属一致。共享 bridge 捕获的输出地址仍仅用于输出存储，不能作为调用身份；缺失/未知绑定不发送请求，模型同名参数无效。MCP Apps host lease 的续调用显式保留自身会话/turn 与可见工具面，使用同一准入而非无身份旁路。
- Workspace owner 的 snapshot/changes 显式返回可信 scope 接纳的 `initiatorSessionId`。冷客户端从该字段重建独立目录；未知、空值或与发现 scope 冲突报 `Unroutable` 并保持对账重试，不生成 root 提醒。删除登记和 metadata 的 root fallback 分支；收件目标未加载时保持失败并在原目标重试。
- 登记与终态恢复共用不可变 initiator 守卫：相同身份允许重放、未知发现不覆盖已知路由，冲突已知身份在修改回调或终态前拒绝。冷恢复先保留 `pending_delivery` 绑定再等待投递，不伪造 Started/执行中事件，避免投递等待期间被并发登记改道；失败或 Future 丢弃仍可重试，已知身份补全同时更新投影与路由。
- 回归入口：`mcp::client::output_store::tests`、`mcp::client::subscription::tasks::{task_projection_tests,task_recovery_tests}`、`agent::async_tasks::{external_settlement_tests,tests::external_initiator_tests}`、Workspace `workspace::tasks_wire_tests`。覆盖捕获 root 的桥接器、伪造参数零效力、无父 runtime 的终态/运行中发现、未知/冲突隔离、目标不可达与同 ID 重试、登记/恢复冲突竞态。
- 验证：四 crate 全量 lib：ACP types 521 passed；Agent 919 passed；Workspace 422 passed / 2 ignored；Middlewares 1566 passed / 2 ignored（两个真实 120 秒期限/取消用例均通过）。相关四 crate doc tests 11 passed / 5 ignored；包含 ACP 的五 crate all-targets clippy（`-D warnings`）、fmt、22 条层依赖规则及 diff 检查通过；相关文档 33 个本地链接目标存在。修改的 29 个 Rust 文件最大 934 行；全库大小扫描仍有 13 个未触碰的存量超限测试文件。
- 集成首轮发现冷恢复误报 Started，以及旧 MCP 测试夹具没有可信会话绑定；等待工具启动的旧夹具套件被中止，修正为真实绑定后完整复跑通过。没有放宽期限、单次调用、无重放或进程收尾断言；冷恢复待投递状态保留独立验证。
- 边界：本步验证新客户端/目录从仍运行的可信 owner 重建直接归属；owner 的 scope/结果记录仍在内存，不承诺 owner 或全服务进程重启恢复，也未实现持久 InvocationBinding/Inbox、授权材料重建、epoch 投递接纳和统一自动激活。未知旧 owner 记录阻塞对账而非兼容 root。第 3–5 步和 P0 发布门槛保持未完成。

## 实施工作

- [ ] 定义稳定调用/任务/事件/投递/批次契约与可信会话能力，统一主子调用绑定。
- [ ] 增加调用意图、可靠 Inbox、处理义务、关闭/暂停状态与 outbox/inbox 存储契约；覆盖本地/远端后端。
- [x] 任务按发起会话独立登记和发现；树级聚合不参与路由，删除 root fallback（第 1、2 步；跨进程持久恢复仍列于第 5 步）。
- [ ] Receive 幂等领取/投影；批次与执行检查点对接，避免非幂等工具重放。
- [ ] SDK 调度消费统一准入与 pending 查询；有界恢复扫描、退出原子交接及暂停协议。
- [ ] 各生产者切换统一发布，删除内部旧分支，不新增兼容 shim 或双权威。
- [ ] ACP/TUI 暴露 Accepted/Projected/处理及阻塞状态，保留用户待发区契约。
- [ ] 按权威 §7 完成主/子、崩溃、乱序、Stop、关闭、容量和投影恢复矩阵。

## 设计审计阶段验证边界（实施前）

本轮仅文档工作；没有运行消息故障复现或 Rust 行为测试。代码可能有并行修改，早先静态调查仅解释设计动机，不作为当前代码缺陷已经复现的结论。文档链接、差异检查和对抗评审结果在下节记录。

## 对抗评审

两路只读 subagent 对抗已完成，分别覆盖会话归属/生命周期与持久化/调度恢复；共发现 6 个 P1，均经权威修正和原评审者复核关闭：

| 反例 | 权威修正 |
| --- | --- |
| 旧 Stop 暂停 Resume 后的新执行 | command ID、目标三元组、预期 lifecycle revision 原子裁决；重复返回原回执 |
| Cascade 子会话被迟到结果复活 | 按委托/工作关联和控制代际持久暂停；自然 attempt 切换不逃逸；实际取消精确定位 |
| 旧委托取消误伤新工作、Independent 阻塞父关闭 | 委托身份与执行关联；终态幂等；显式 detach/abandon 与迟到结果处置 |
| 输入 Satisfied 后未完成 Act/Reason 永久停住 | Satisfied 原子移交持久阶段责任；调度集合包含阶段续接 |
| Passive 满载拒绝驱动消息形成死锁 | 独立配额/保留容量或等价可证明进展机制 |
| 清理去重后全量发现重复处理 | 去重覆盖重试、发现、重放、备份恢复；退休水位/持久拒绝证据 |

生命周期评审在首次复核指出 Cascade 的自然 attempt 换代仍可逃逸，补充关联控制代际后再次复核通过。存储评审首次复核通过。通过仅指本轮文档反例闭合，不代表无其他缺陷或实现已验证。

实施额外验收：

- [ ] C 完成 D1 后执行 D2，迟到 cancel(D1)/Stop 不影响 D2；同一 D 的自动 attempt 换代不能逃逸 Cascade 暂停。
- [ ] 父 Close、Independent 子会话继续完成，验证 detach 后父关闭和迟到结果留存。
- [ ] ContinueCurrentRun 的阶段责任在崩溃换 attempt 后明确恢复/阻塞/抑制，不能无限 Pending 或暗中升级 EnsureProcessing。
- [ ] 容量按消息数、字节、产物和事务资源验证，不以保留一个槽位冒充进展保证。

文档检查：`git diff --check` 通过；针对本轮 11 份文档的本地链接目标检查通过，共核对 63 个引用（仅验证目标路径存在，不验证章节 anchor 或外部 URL）。已核对标准、设计索引、Agent 模块指引、代码索引及旧 active issue 的替代路由。未运行 Rust 测试；未修改或提交实现代码。

## P0 对抗结果与证据账本

### 范围和裁决

四路初审分别从身份授权、持久责任、调度控制、跨层落地发起对抗，报告 15 项（有重叠）；其中 A3 为 P2 授权材料补充，其余按 P1 阻断审查，不因项目风险为 P0 而将所有发现升格 P0。交叉复核另发现控制代际歧义 C6、存储未应用最终性 E1，以及 D1 的发布身份映射缺口。下表按故障类合并，保留原编号以追溯。

| 编号 | 反例与不变量 | 权威处置 | 本轮证据 |
| --- | --- | --- | --- |
| A1/C5 | 旧创建/结果穿过 Close-Reopen | §8.1 生命周期绑定覆盖 create/publish/discovery，旧结果不激活新生命周期 | 控制评审交叉复核；epoch 构造例 |
| A2 | best-effort owner 无 barrier 无法证明排空 | §8.6 能力准入、OutcomeUnknown/Incomplete，本地停止不冒充外部停止 | 安全约束复核；默认政策仍阻断 |
| A3 | 子恢复沿用父/宿主更宽权限 | §8.1 持久授权引用/上限，恢复重新授权、失败 Blocked | 控制评审按原反例复核；无运行时证据 |
| B1 | 一个 event 生成多个 delivery 再处理 | §8.1 逻辑唯一键与受控 purpose | 集合构造例、跨层评审复核 |
| B2 | 响应未提交先执行工具，恢复重新 Reason | §8.2 副作用前持久屏障与稳定调用身份 | 6 种 commit/dispatch/crash 顺序探针；交叉复核 |
| B3/C3 | D2 通过共享 Transcript 执行已暂停 D1 | §8.4 循环间数据隔离 + 投递资格过滤 | 过滤构造例；用户裁决后仅保留循环间数据分离/投递准确验收目标 |
| B4 | Accepted 后 owner 释放，恢复旧备份丢唯一责任 | §8.6 声明故障域/RPO，域外 DataLoss 不冒充可恢复 | 备份回退构造例；部署证明待提供 |
| C1/D4 | 唯一实例内并发 loop；旧票据穿过 Pause | §8.3 同会话 attempt 准入、执行入口重验、静止回执 | 6 种激活顺序与负控；串行准入构造例 |
| C2/C6 | 旧 Resume 或新 D2 解除新 Stop/D1 暂停 | §6.3、§8.3 域内控制代际及全部控制幂等 | 控制评审复核；命令/域构造例 |
| C4 | 每个 attempt 重置预算形成无限自动重启 | §8.3 稳定工作预算持久累计 | 预算构造例、复核 |
| D1 | 撤回仅删内存，恢复重投；重发撞旧事件键 | §8.5 持久撤回/领取裁决，input+发布代际确定 event | 构造例、身份映射复核 |
| D2 | 旧 Transcript 无 ACK，升级猜 Pending/Satisfied | §8.6 LegacyUnknown、格式准入、停旧写路径、回滚责任迁移 | 跨层静态复核；迁移实验未执行 |
| D3/E1 | 提交 Unknown 后重 Reason；无回执误判未提交 | §8.2 同 mutation 重试/回读，NotApplied 需阻止迟到提交的最终性证据 | Unknown/延迟提交构造例、独立验证复核 |

### 探针与独立验证

可重复命令：`python3 scripts/rcra_message_contract_probe.py`。该脚本不调用产品接口、不修改会话数据、不需要凭证。

- 第一轮 11 项通过，但独立验证指出 activation 安全计数含恒假表达式，不能证明 revision 守卫有效；该证据撤销，不用于通过结论。
- 第二轮改为显式 start 状态迁移，增加删除 revision 守卫的负控，并新增 E1/C6 构造例。主 Agent 和独立验证者分别运行，最终 **13 项通过**。
- 只有 dispatch 与 activation 两项各枚举 6 种有限顺序；其余 11 项为构造示例。原子性、存储持久性及模型行为均是未验证假设。
- 这些证据支持具体反例的可表达性及补充约束在抽象模型中的作用，不支持完整状态空间安全、调度活性、真实存储提交、跨进程执行隔离或模型输入隔离。
- E1 的服务端终结记录阻止迟到提交分支必须在真实后端验收；当前探针只覆盖无最终性证据时不得创建替代响应。

### 产品裁决（2026-10-05，用户）

1. **不引入控制域合并议题**：主 Agent resume 子 Agent 属正常 MQ 流程；判定我此前的追问为过度设计。设计重心回到循环间数据分离与消息分级投递准确性。
2. **best-effort 默认允许，重复执行可接受**：允许能力不足的 owner 后台运行；至少一次语义。仍须诚实报告未知状态，不伪造完成/取消。
3. **MQ 消费语义统一**：消息行为统一到 MQ——类型在发布时确定，loop 在 Receive 领取时按类型决定行为；运行资格以 MQ 可运行状态为唯一源真相，唤醒降级为通知。权威 §4 已按此重写，替代散落的 continuation/idle 特例描述。
4. **停止、关闭与连接模型（场景评审修正）**：停止子 Agent 及其后台执行的正确机制是关闭该子会话——关闭级联终止其 bg shell，资源终止并结算后才向主 Agent 返回消息；用户可直接关闭子 Agent，主 Agent 对自己发起的子会话经关闭协议发起。连接模型视为可靠连接：不按断线重复投递设计，同一事件按身份幂等；重复执行仅在真实二次执行时出现且允许。

两项原"未决阻断"关闭，实施准入由 BLOCKED 调整为"按下方审计缺口继续修正后实施"；裁决 3 已并入权威 §4（消息类型与 MQ 消费语义），裁决 4 并入 §5.1、§6.3 与 session-async-tasks §5。

### 实施前关键目标审计：循环数据分离与分级投递准确性（2026-10-05）

审计对象为目标本身"被解决没有"，覆盖工作区当前代码（含已暂存改动 `manager.rs`/`registry.rs`）。用户确认场景：主→resume sub 正常；重点是每个 loop 的数据分离与投递准确。

**已解决（有代码证据）：**

- 子会话拥有独立 MessageQueue、Transcript、cancel 与 turn（`factory/context.rs:58`、`v2_bridge.rs:256-263`）。
- 在线路径投递归属 = 直接发起会话：`tool_bridge.rs:322-328` 从可信 `ToolContext.session_id` 取 initiator；scope owner 独立保存；投递路由用该会话自己的 transcript+queue（`tool_execution...execution.rs:327-336`）。
- 已暂存守卫阻止 scope/终态对账把已记录的 initiator 改道 root（`manager.rs` staged diff、`registry.rs::external_initiator`）。
- 分层语义正确：Defer 唤醒、Info 不唤醒（`queue.rs`）；终态仅一次（terminal transition ID）。

**未解决（即用户报的"没有触发"）：**

- **子会话缺少再次运行机制**：child 未注册 `session_inboxes`/`session_tasks`（仅 ACP 顶层 `requests.rs:162-177`、`bridges.rs:77`）；子 loop 不启用 idle 等待（`v2_bridge.rs` builder 无 `with_idle_should_wait`）；`continuation_notify` 仅顶层 prompt 路径（`peri-acp/src/host/prompt.rs:539`）。结果：MCP 任务终态写入子 transcript 与子 queue 后，子 loop 已结束，无人再消费 → 消息"只落库不处理"。
- **冷恢复/scope 发现仍 root 兜底**：`subscription_tasks.rs:187-190,929-932` 在 initiator 不可得时投递 root 并标 `root-fallback`。用户已接受重复执行，但归属准确性目标要求重建发起者；当前重建缺口存在。
- **子会话任务目录未分离**：子发起的 MCP 任务登记在 root 的 TaskManager（`session_id` = `mcp_task_owner_session_id`），child 无自己的任务目录/接收通道——与"一等公民数据分离"仍有差距。
- **嵌套委托登记到祖父会话**：嵌套 spawn 使用克隆的 SubAgentTool，其 `parent_session`/host cell 指向祖父会话（`spawn_context.rs:130-135`、`define.rs:58-61`、`stage_builder/subagent_setup.rs:44-83`），父子链跳层——嵌套场景下数据分离被破坏。
- **子代工具面克隆父实例，回调绑定未验证**：fork/bg/resume 子代克隆父工具 Arc（`execute_fork.rs:42`、`execute_bg.rs:48`、`execute_resume.rs:100`）；bg_complete 回调按构造时 session lazy resolve（`bg_complete.rs:38-61`）语义正确，但生产子代收到的回调版本与归属在本轮静态审计中未验证。

### 发布门槛（均未完成）

- [ ] 本地/远端 SessionResources 提交与 ACK 各切点注入故障，验证 mutation 回执最终性、Unknown 热态冻结和无重复 Reason/Act。
- [ ] SDK 同实例多入口与跨实例替换实验，验证只有一个可推进 attempt、暂停与交接排序及无旧实例副作用重放。
- [ ] MCP 创建响应丢失、发现乱序、scope epoch、close barrier、owner 重启和产物保留的真实契约测试。
- [ ] 主/子/嵌套/Workflow 等价 E2E，父 runtime 消失后子独立恢复、结果归属、委托回执和授权上限。
- [ ] Stop/Resume/Close/Reopen、撤回/重发、循环间数据分离与投递归属 E2E，包含错误授权及迟到消息。
- [ ] Compact/fork/rewind、旧数据升级/回滚、配额/大 payload 与去重退休/备份恢复验收。
- [ ] 按实际部署提供故障域/RPO 和恢复证据，不以抽象探针或文档审查代替。
- [ ] MQ 消费矩阵验收：类型 × 到达时机（运行/空闲/退出/重启）× 会话状态；含 required 不降级、Passive 不阻塞 required 接纳、子会话与主会话同构。

### 设计审计阶段结论（实施前）

文档静态检查：`git diff --check` 通过；本轮涉及的四份设计/任务文档共 17 个本地链接目标全部存在（未检查 anchor 或外部 URL）。探针 179 行。未修改生产实现、未执行 Rust/TUI E2E、未提交；已有其他工作区改动未纳入本轮结果。

**实施准入：两项产品裁决已由用户关闭；按"关键目标审计"缺口继续修正。** 循环内 MQ/Transcript 数据分离与在线投递归属已有代码证据；子会话"只落库不处理"（无 idle/无 continuation/无 inbox 注册）、冷恢复 root 兜底、子会话任务目录仍挂 root 三个缺口未修。真实存储、SDK、MCP 故障注入与端到端门槛仍待实现后验证，不能据文档或抽象探针宣称 P0 验收通过。
