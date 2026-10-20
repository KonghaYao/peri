# Session 异步任务

- 状态：**执行恢复已剥离，当前任务生命周期保留**；验证范围见剥离计划。Child 有界收尾 A′ 已实现并通过定向/全库回归；真实 MCP wire、TUI 手测与进程重启自有 fixture 仍未覆盖，见 [active spec](../../spec/issues/2026-10-20-p1-sync-subagent-unbounded-settlement.md)。
- 消息与运行生命周期服从 [RCRA 权威](rcra-message-activation.md)；实施状态见[剥离计划](../../spec/issues/2026-10-07-remove-execution-recovery-plan.md)。

## 1. 归属与当前运行

每个会话拥有当前进程的任务目录、队列和取消关系。共享 MCP 连接不共享含混的收件人回调；结果投递给直接发起会话，父子关系仅表达委托与显式取消策略。

模型 tool-call ID 用于工具卡及子 Agent 展示归属；当前 invocation/task ID 用于当前调用关联。当前运行可保留可信绑定和去重，但不持久化 task binding、owner declaration、恢复 locator 或 terminal delivery ACK 账本。

## 2. 操作与结果

保留当前任务的启动、查询、等待、进度、结果和取消。TaskManager、MCP task subscription 与当前队列是运行期实现，不提供跨进程恢复保证。

完成结果按可信来源进入当前会话队列或既有 canonical reminder 路径，保留消息持久化与工具/任务错误可见性。展示成功不代表工具成功，取消请求已发出不代表外部资源已停止。Workspace 结果文件和产物引用保持原有访问和有界投影语义。

当前会话宿主持续监听队列激活信号；主 run 自然结束（包括有界等待到期）不撤销该监听。结果晚于 run 退出到达时，仍经原 continuation scheduler、prompt lock 和代际校验启动新 run。监听在订阅后复查队列，run 结束也复查队列，覆盖退出与投递竞态；活跃 run 自己消费结果，不额外并发启动。Info 或已消费的消息不产生空跑，失败不自动反复重试同一输入。

执行失败仅阻止同一输入集合自动重试，不永久关闭会话的后台激活权限。以尝试开始时的队列接纳序号为边界：边界后到达的 Required / EnsureProcessing、模型可见消息可重新激活，包括尝试执行期间或失败之后到达的任务错误结果。Passive、Info 默认策略或仅 UI 可见消息不解禁旧输入；Error 严重度本身不授予调度权。显式 Stop 与会话关闭仍优先，不因新错误结果重新放行。实施记录见 [异步错误激活修复](../../spec/issues/2026-10-08-async-error-notification-activation.md)。

本地队列接纳与 TaskManager 终态结算同一回调完成，不把投递放进无人等待的 detached task。显式取消普通 prompt 保留原有一次独立子任务结果续跑；取消正在执行的 continuation 不再次自动续跑，剩余结果留给后续用户输入。会话关闭撤销监听并排空任务，不允许迟到结果重启关闭的会话。此机制仅属于当前进程，不恢复旧执行或持久化唤醒义务。

同步接纳拒绝或 panic 时，owner 保留原结果、冻结路由和 delivery ID，由同一 ExecutionScope 的单一 worker 自动退避重试，不要求新用户输入。重试只重新接纳结果，不重新执行工具或模型；同一 ID 的相同载荷不重复入队或唤醒，冲突载荷明确拒绝。worker 使用弱 owner 引用，关闭 scope 撤销它；永久拒绝保留 pending，关闭排空如实报告未结清，不伪装成功。

用户输入、已批准的定时执行和后台结果续跑具有显式且不同的宿主准入来源；定时任务的审批许可不依赖后台结果的自动续跑开关，也不重新放行已被停止的被动续跑。运行请求的 MQ 消费语义不从可变 pending 标记推断。

MQ pending 标记是后续处理提示，不作为排队请求的准入锁。重复通知经 prompt lock、代际和队列复核收敛；队列已被消费的排队请求直接空跑返回，不调用模型，也不能阻断之后到达的新结果。续跑派发与宿主任务准入失败须记录会话身份及错误原因。

续跑在 prompt lock 内持有同一个 sessions 临界区，复核 epoch、closing、runtime、队列和 activation 后，同时提交 in-flight 与取消令牌。先到的用户输入使旧代际请求失效；已准入的续跑令牌可立即被 Stop 取消，不能在实际执行时换成新令牌。尚无执行令牌的 Stop 仍撤销排队代际并抑制激活。

## 3. 移除冷恢复

不在 session/load/resume、新输入或进程重启时扫描旧 task scope、重建旧 invocation、接管 owner 或自动续跑 child。缺少旧 callback/任务身份不写 Work quarantine，不阻塞新的普通执行。

历史状态可显示，但不是当前 running。重启后不保证外部后台结果再次送达；外部任务可能继续存在，显式工具查询或人工处理不等于恢复旧 Agent 执行。

## 4. 取消与关闭

当前进程保留 Cascade/Independent 策略、取消 token、MCP owner 关闭顺序和有界排空。清理未结清明确报错，不假称已停止。

取消本地后台 Agent 后，owner 的协作退出仍须交付原 task ID 的一次结果；这不是第二次 registry 终态，也不绕过父会话 Stop 或关闭。强制 abort 没有结果时不得伪造已完成清理。后台 resume 先完成资源认领移交再放行执行；放行前失败由资源认领 handle 恢复原记录，已运行后台的终态不能被前台终止声明覆盖。正常结束与 panic 均先收尾事件、hook 和会话状态，再交付 owner 终态；状态落库失败是可见失败。

显式会话关闭意图可以独立保存；它不保存旧 Agent 阶段或恢复任务目录。关闭历史会话不自动发现/接管其旧外部资源。

## 4. 当前进程执行链

当前实现由 child runner、宿主 execution 通知及 TUI lifecycle 共同履行以下契约；定向验证范围与待完成的真实使用验收见 `spec/issues/2026-10-08-async-execution-chain-fixes.md`。

- Child 的 bounded idle 只结束一次计算推进，不等于委派成果终态；存在未结任务时，当前进程必须仍有可响应取消和结果唤醒的执行责任方。结果留在直接发起 child 的队列，不转投根会话，也不建立持久恢复机制。
- Child 的事件转发失败是可见失败，不能让已退出的 worker 留在 Running；保留原失败原因，可靠接受一次终态后结算并释放当前运行登记。
- 内部 continuation 和定时审批具有明确的宿主交互身份；开始通知必须先于反向交互，结束通知只关闭相同身份的交互许可。用户输入身份不能隐式充当所有执行身份。
- Suspend 与恢复后的输出具有可判序的生命周期关系；陈旧终态和迟到提交失败不能清除其他会话或新执行的 loading、消息缓冲或交互许可。
- 输入回滚文本绑定其提交请求，不绑定当前显示的 execution；审批或内部续跑的终态不能删除或恢复另一个待执行请求的输入。只有 managed 输入的 Stop 携带 mailbox 票据身份，external execution 仍走宿主普通取消入口。
- 验证从完整行为出发：嵌套 child 晚到结果、forwarder 失败、内部续跑首步 HITL、空闲定时审批、suspend/wake 事件交错和跨会话迟到提交错误。

120 秒仍是一轮 RCRA 的 idle bound，不是 Task 工具的整任务 wall-clock timeout；child 另有 §5 的单次执行收尾窗口。当前进程的 child owner 保留结果消费责任，取消、关闭与累计语义迭代预算仍约束该执行。

## 5. Child 有界收尾

同一次 child 执行在首次 RCRA 推进结束且仍有任务或消息处理义务（active task、pending delivery、未消费 Required 结果）时启动唯一窗口，`D = t0 + HANDOFF_MAX_WAIT`（120 秒）。activity、结果到达、短暂空闲、任务新增和 loop 重入均不重置 D；同一 `ChildSettlementWindow` 由 loop、runner 与 sync wrapper 共用。显式新 run（resume）创建新窗口。

loop 在 cancel 检查后、Receive 前检查 D；Receive 之后、新语义迭代准入前再次检查，覆盖持久化等待跨过 D 的情况。Receive 无条件返回本轮已 canonical 提交、且本应驱动语义迭代的 Process 消息身份；loop 在判定 `SettlementDue` 或 HookStop 时统一登记为未推理义务，避免 Receive 内部到期检查与登记之间的 check-then-check 竞态。D 前已准入的 Compact→Reason→Act 完整结算，不丢弃 tool-call/result 配对；D 后不再准入新迭代。settlement `select!` 使用 `sleep_until(D)`，不依赖 activity 恰好到来。到期以 `LoopResult::SettlementDue` 返回，不调用 turn cancel token，也不取消 Shell。

全部任务、投递与消息义务结清才是 `ChildRunOutcome::Settled`；否则返回带 child/execution、资源 owner、任务与消息引用、canonical 消息 ID 的 `HandedOff`。`Failed` 保留原错误；若仍有资源义务则附残留 receipt。执行错误后交接写入失败返回 `HandoffFailed`：原错误与结构化 `handoff_persistence_failed`、冻结 receipt 同时保留。child 元数据写 Error（`child_tasks_handed_off`），父结果 success=false、timed_out=false，保留本轮已产生文本，不把未完成显示为成功或 Running。

轻量资源 owner 由父 `TaskManager` 持有，保留原 manager/MQ、canonical 写入端口、冻结交接快照及执行准入；不保留模型、StageContext 或旧事件总线。handoff/resume/close 共用执行许可：`prepare_resume` 阻止并发 resume，`bind` 阻止 close 与 handoff 竞争，permit 在状态提交和旧 Stop/事件排空后释放。交接在 flush 后按稳定 ID 幂等 append，提交成功后才登记 receipt；失败显式报告并保留 pending 身份供重试。`has_unsettled_obligations` 计入 failure 与 pending receipt；未确认交接拒绝 acquire。close 先关闭真实资源；若存在未确认交接/执行失败，则保留 owner/binding、跳过消息结算并返回 Incomplete。取消认领在资源关闭后仍需写入 Cancelled，避免线程永久 Active；真实资源关闭失败则保留 Active。close 的 history flush 区分 writer 正常关闭与真实持久化失败：后者返回 Incomplete 并保留 owner/binding，不静默释放。

sync wrapper 只在收到已提交 receipt 后 detach；无父投递宿主时等待 typed outcome 并同步返回未完成，不制造幽灵任务。父结果投递失败由 registry `pending_deliveries` 的单一 retry worker 接管，wrapper 释放完整 child session，不再自建轮询；重试成功回执清除此前的 delivery 失败记录。background 与 sync 一致：事件排空失败进入执行 permit，阻止新 run 准入，而不只是改写结果文本。执行/交接已失败后的 history flush 失败不得把 `HandoffFailed` 替换成无 receipt 的 `Failed`；保留原错误、结构化提交失败与冻结身份，flush 错误只记录。

截止后的结果仍投直接 child 的 MQ；不隐式复活旧执行。新的模型分析需要显式 `Agent(resume_thread_id, prompt)`，这是长任务超过窗口的已接受代价。旧执行的 ContinueCurrentRun 控制消息在新 run 中被抑制；旧 Stop 在 owner 开放下一次准入前排空。

不 resume 而关闭时，owner 先阻断 resume，取消/结清真实资源，再由非模型路径将停驻 MQ 中的终态按原 delivery ID 写入 child canonical；其他未处理输入记录为 `child_retained_input_unprocessed`，不静默清空。失败保留可查询 owner 并返回 Incomplete，不释放 binding，不宣称 Closed。

不新增 DB 表、持久状态枚举或 ACK 账本；不扩张 late result 即时落库保证。进程重启仅加载已保存历史，不恢复旧执行或尚未持久化的内存结果。

## 6. 验收

覆盖当前任务结果/进度/取消、父子消息路由与展示身份、连接关闭失败和新输入处理。覆盖历史加载不触发任务发现或旧副作用；不再验收冷恢复、永久 terminal ACK 和跨进程处理义务。

Child 收尾回归使用 paused-time：永久 Working 无通知、activity/任务新增不重置 D、D 前/后/同刻完成、task 终态但结果未消费、Receive 跨 D、已准入 Act 完成、预算/HookStop、cancel 与 D 同刻、sync/bg × 新建/resume、父注册竞争、交接/状态写失败、late result 后 resume、close 消息结算、A→B→C 归属。真实进程死锁/进程树清理与 TUI 手测仍按仓库 testing 规范另行验收。

TUI 后台验收使用隔离 HOME、真实二进制与本地模型重放：只提交一次用户输入，核对当前 canonical reminder、直接父会话后续模型请求及回答、child 终态和重复投递。验收只读取当前 threads/messages，不依赖已删除的 Work 表。
