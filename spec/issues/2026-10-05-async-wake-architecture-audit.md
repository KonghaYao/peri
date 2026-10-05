# 异步任务投递与 loop 唤醒架构审计

状态：用户于 2026-10-05 授权开始收敛；P0 mailbox 与 owned 终态结算已落地并通过本轮验收。P1 生命周期调度与跨进程恢复仍未完成。

审计日期：2026-10-05；调查基线：`0cc312cd`。下文 F1–F5 描述调查时的缺陷，不代表整改后的当前实现。

## 当前收敛范围

- 已统一 mailbox 数据与 wake owner；去除 producer 可选 inbox_handle 和独立 idle receiver 身份。直接发布、batch、重复包装与已挂起 waiter 使用同一行为契约，Info 不驱动 loop。
- 已将 owned Shell/Subagent/Workflow 的交付后结算移至 TaskManager；回调返回确认，失败/panic 保留原结果和 delivery_pending，支持 owner 重试。missing inbox 不再 no-op；生产 Workflow router 必填，有 parent 的后台 Subagent 有默认投递路由。
- 已统一终态稳定 delivery ID 与现有 Receive 去重；关闭重试 pending，pending 或交付 in-flight 都不能假装排空。取消 Shell cleanup 不产生第二次终态。
- 外部持久提交交付契约保留；默认 external settlement 不再绕过交付直接 complete。Info severity、被动 MessageKind::Info、取消不链式和 epoch 政策不变。
- 本地 pending 仍为进程内保留；当前重试触发为新 prompt、关闭排空或显式 owner 调用，没有自动重试 scheduler。P1 的统一 continuation owner、冷恢复 pending 与全链路诊断仍需后续任务，不能把当前 mailbox 接纳描述为 durable 已送达或模型已处理。

现行设计见 `docs/design/session-async-tasks.md` §3；入口见 `docs/code-index/peri-agent.md` 与 `docs/code-index/peri-acp-types.md`。确定性探针已替换为正确行为的正式回归，不保留“未唤醒应通过”的测试。

本轮验收：主工作区 `peri-agent --lib` 895 passed、`peri-acp-types --lib` 521 passed；Agent/types doc tests 合计 11 passed、2 ignored；`peri-middlewares`、`peri-acp`、`peri-mcp-common` 的 all-targets check 通过。Mailbox 正确行为回归在修复前 4 failed/1 passed，修复后 5 passed。Workspace worker 验证回调迁移后全量 lib 422 passed、2 ignored。未因本次提交再重复整 workspace 测试。

## 结论与证据边界

反复出现的不是一个 `Info` 枚举值问题，而是同一类责任泄漏：调用方必须同时记住业务消息是否驱动模型、使用哪个投递入口、唤醒哪一个等待者、何时结算任务、loop 退出后是否另行调度。现有 Interface 不能确保这些动作保持一致。

已通过源码和确定性探针证明：**`MessageKind::Defer` 与 `queue.has_wake_up() == true` 都不保证已挂起的 inbox 被唤醒。**这解释了为什么只改消息分类或修复某个 producer，不能封住同类回归。

本次没有现场会话日志，不能认定用户所贴 shell 提醒对应哪条失效路径，也不将 Interface 的可构造错误状态都描述为已经发生的生产故障。

需要纠正上一轮归因：`d86fff1a` 在 2026-09-16 引入 active subagent 补充消息时，明确设计为被动 `Info`，并测试“末轮不额外触发模型”。`0cc312cd` 将它改为 `Defer`，改变的是这项原有契约；它不是 shell/subagent 终态分类退化的复现，也没有统一底层投递机制。

## 历史：同一种不变量在不同入口反复被修补

| 日期 | 提交 | 已核实的修复内容 |
| --- | --- | --- |
| 2026-06-28 | `38fe98d7` | 撤销后台结果 Defer→Info 的错误方案；修复已 drain 的 awakened messages 未写 transcript；修正后台 Started/Done 顺序。 |
| 2026-07-11 | `a1b44f5b` | 任务 active count 先归零、结果后经异步事件泵入队，loop 提前退出；改为在 registry complete 前同步回调投递。 |
| 2026-09-02 | `86b7ec3f` | Workflow 快速失败又在 Defer 入队前 complete；删除该路径的提前 complete，交给通知 consumer 排序。 |
| 2026-10-05 | `3c3d9157` | canonical 终态交付只 push 原始 queue，未通知 inbox；新增可选 InboxHandle，并在有 handle 时通过它投递。 |
| 2026-10-05 | `0cc312cd` | active subagent 补充任务改为 Defer，补齐末轮续跑测试；仍直接 push queue。 |

7 月和 9 月是同一“结果入队前不减少 active count”不变量在 Subagent 与 Workflow 的不同实现中分别被破坏；10 月则显示“入队”和“唤醒”仍可以被分离。共同原因是跨入口依赖调用约定，而不是由一个 Module 封装生命周期。

## F1：队列与唤醒身份分离，合法 Interface 可构造丢唤醒

代码入口：

- `peri-acp-types/src/session/queue.rs`：`MessageQueue` 保存 `inner` 和自己的 `notify`；`push` 只通知这个 `notify`。
- `peri-acp-types/src/session/inbox.rs`：`SessionInbox::new` 另建独立 `wake`；`await_wake` 等的是它，而非 queue 的 notify。
- `InboxHandle::push`：先 push queue，再按消息 kind 通知 inbox wake。

两个确定失效形状：

1. 等待者已进入 `await_wake`，producer 直接 `queue.push(Defer)`：队列可见，但等待者不醒。
2. 两个 `SessionInbox::new` 包装同一个 queue，producer 持有其中一个 handle，consumer 等待另一个 inbox：数据身份相同，唤醒身份不同，等待者不醒。

`await_wake` 的 fast path 会掩盖这类问题：先投递再开始等待的测试可以通过，但已挂起后投递失败。registry watch 或其他 wake source 也可能意外补偿该缺陷，令它表现为生命周期相关、偶发且难定位。

这不是要求禁止所有 spurious wake；关键是有效工作不能在等待者睡眠后只更新数据而遗漏它实际订阅的信号。

## F2：多个写入口和可选依赖让正确性外溢到装配层

现有入口包括 raw `MessageQueue::push`、`InboxHandle::push`、`AsyncRouter`、middleware enqueue helper 和 canonical terminal delivery。

仍可绕过统一唤醒的路径：

- `peri-agent/src/middleware/state.rs`：既暴露可写 `v2_queue`，又提供 `enqueue_v2_message`；helper 在 inbox 缺席时回退 raw queue。
- `peri-agent/src/agent/async_tasks/delivery.rs`：同时接收 queue 与 `Option<InboxHandle>`，两者同源性不受类型约束；缺 handle 时回退 raw queue。
- `peri-agent/src/agent/stages/mod.rs`：idle inbox 与 producer inbox handle 是两个独立可选字段。
- `peri-agent/src/session/exec/stage_builder/dependencies.rs`：分别设置等待 inbox 和生产 handle；正确性依赖装配路径保持一致。
- `peri-agent/src/agent/async_tasks/agent_inbox.rs`：本次改为 Defer 后仍直接 push queue。

限制：当前子 Agent 的 `session/subagent/v2_bridge.rs` 不装配 idle inbox，所以上一轮补充任务回归通过是合理的；不能声称它现在必然发生挂起丢唤醒。但这条生产入口仍要求维护者知道“这里目前不睡眠”的上下文，一旦增加等待能力就可能重复失效。

## F3：任务完成与结果交付仍由不同调用方手动排序

- `peri-agent/src/session/subagent/background.rs`：先可选 `on_bg_complete`，再 `task_manager.complete`。没有回调仍可以完成任务。
- `peri-acp-types/src/tasks.rs`：`OnBgCompleteFn` 返回 `()`，无法向 owner 表达交付失败或待重试。
- `peri-agent/src/session/bg_complete.rs`：找不到目标 inbox 时仅记录 debug 并返回；调用方无法区分投递成功与未路由。
- `peri-agent/src/session/workflow_completion.rs`：router 与 fallback queue 都是 Option；均缺席也继续 complete。

这些是 Interface 允许的错误状态；是否在具体生产装配中可达需要逐入口验证，不能只由 Option 存在推断现场必然丢消息。

对照：外部 MCP task 的 `TaskManager::settle_external` 已有较深的 Module：claim completion → await notification delivery → delivery 失败撤销 claim → 成功才提交终态。Subagent/Workflow 还依赖 producer 自己遵守顺序。现有 `TaskTerminalDelivery` 也已定义 canonical 提交的独立 Interface，不需要另造与它竞争的持久化权威源。

**改进应把已有严格交付契约推广到实际第二、第三个任务用例，而不是再新增一套 callback wrapper。**

## F4：调度政策仍散落在 producer 与 host 生命周期中

同一个 reminder 的业务字段和调度 kind 分别构造，producer 可以给终态任意选择 Info/Defer。`TrustedSystemReminderFactory` 校验的是内容可信性与 envelope，不负责调度政策。

loop 内等待由 `MessageKind::wakes_up` 和 inbox 控制；loop 退出后的消费又通过 `needs_mq_continuation`、continuation 请求、epoch、armed/in-flight 等状态控制。见：

- `peri-acp-types/src/session/queue.rs`：`has_wake_up` 与 `needs_mq_continuation` 的谓词不同。
- `peri-agent/src/session/exec/executor_helpers/v2_execute.rs`：loop 完成后发现队列非空则请求 MQ continuation。
- `peri-acp/src/host/continuation.rs`：普通后台取消续跑只接受 Agent，MQ steering 另走分支，dispatch 时再次检查 eligibility。

这里需要保留合理区别：Info 队列非空可以要求 transcript 对账，不等于应调用模型；Receive 本身已经按 wake_up_count 阻止 Info 单独驱动 Reason。取消后不链式续跑、epoch 失效也是明确安全契约，不能为统一入口而删除。

结构性风险是这些政策分散且需要跨层配合；单独证明 kind 为 Defer，不能证明从挂起、退出、取消或冷恢复状态都能正确推进。

## F5：展示与测试无法充分证明执行交付

`peri-tui/src/kit/message_area/render/user.rs` 的提醒标题取 `data.severity`。`Task · shell · Info` 只表示正常级别的 Task 提醒；不能据此判定队列 kind，也不能证明对应结果已被 loop 或模型处理。

部分路由测试只断言 queue 长度或 `has_wake_up`，无法捕获 F1。当前已有真正等待唤醒的 terminal delivery 测试与完整 loop 生命周期测试，应成为所有 producer 的共享行为契约，而不是只保护刚修过的入口。

需要区分四个可核对状态：canonical 已提交、可执行工作已发布、Receive 已消费、模型已处理。TUI 展示完成与 task projection 完成不能替代后三项。

## 确定性探针结果

临时 integration target：`peri-agent/tests/async_wake_architecture_probe.rs`。

执行：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --test async_wake_architecture_probe`。

4 项断言通过；这些是调查探针，失败路径用“保持未唤醒”的断言证明缺陷，并非声称生产契约测试全部正确：

| 探针 | 观察 |
| --- | --- |
| direct Defer → 已注册 waiter | queue.has_wake_up 为 true，但等待超时。 |
| 对应 InboxHandle 的 Defer → 已注册 waiter | 正常唤醒。 |
| 同 queue 的另一 inbox handle → 已注册 waiter | queue 可见，但等待超时。 |
| Info → 对应 InboxHandle → 已注册 waiter | 不唤醒，符合被动通知契约。 |

探针先用 `futures::poll!` 确认 waiter 已返回 Pending，再投递；不依赖 sleep 猜测等待者是否已经挂起。错误路径等待上界为 25ms，正向对照为 100ms；这些数值是断言截止时间，不是性能测量。

临时源码保存在 `/tmp/peri-async-wake-audit.fxsKHL/async_wake_architecture_probe.rs`，调查后移出仓库，避免把已知缺陷行为固化为应保持的测试契约。该临时文件不是长期事实源。

## 收敛分层：先封住错误状态，再迁移入口

1. **P0：统一 mailbox 的数据与 wake 身份。** queue、producer handle、consumer wait 从同一个 owner 派生。所有异步投递走唯一发布操作；禁止或内部化绕过唤醒的 raw 写入口。若保留多个 receiver wrapper，它们也必须共享同一投递状态与信号。消费/等待时检查有效工作，保留 Info 不驱动模型的语义。
2. **P0：统一任务结算。** 复用 TaskTerminalDelivery 与 external settlement 的严格契约，让需要通知模型的本地 Subagent/Workflow 同样由 owner 负责“交付成功后发布终态”。没有 route 或交付失败不能假装完成交付；明确保留待交付状态并支持重试。无活跃 loop 时持久提交即可证明送达，不承诺已经执行。
3. **P1：由 typed 业务入口决定调度。** task terminal 与 parent supplemental message 使用明确的业务操作，不要求各 producer 重选 MessageKind；通用 MCP subscription 的远端声明在 adapter 边界转换和校验。severity 与调度保持正交，不把所有 Task 或所有 Required 提醒强制转成 Defer。
4. **P1：把 loop 存活与重启政策归入 session 级执行 owner。** producer 只发布工作，不自行决定 idle wake、host continuation 或任务类型特判；执行 owner 维护同一 ready-work 事实并处理 cancellation/epoch。不可简单允许所有 Shell/Workflow 在取消后自动重启，因为那会改变现有用户停止契约。
5. **P1：行为契约与诊断闭环。** 所有真实 producer 跑相同的运行中、已挂起、退出竞争、取消、重复交付和恢复矩阵；记录交付 ID、归属 session、调度政策、canonical commit、Receive 消费及续跑结果，能区分“已展示”“已存储”“已推进”。

## 验收要求

- 已进入 Pending 的 waiter 之后，所有可执行投递都能推动正确 session 的 Receive；不再依赖其他事件偶然唤醒。
- 同一 mailbox 的发布与等待不能因重复包装或装配缺字段而分离；无效组合无法构造，或在装配时显式失败。
- 必须通知模型的终态不接受被动调度；诊断 Info 保持不单独驱动推理。
- 任务执行终态与交付结果明确区分；缺 route、提交失败和重试不能静默吞掉结果或产生第二次有效模型处理。
- loop 已结束与取消场景使用明确生命周期政策；保持取消不链式、epoch 失效和冷恢复去重契约。
- canonical 存储、外部 MCP task、Subagent、Workflow 不产生新的双写权威或兼容旁路。

本任务已修改生产实现并完成 P0 验收；用户已授权快速提交。P1 未完成项仍保留在本 active issue，不作为已完成能力；提交身份以 Git 历史为准。
