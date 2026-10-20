# P1：子 Agent 无界收尾

状态：A′ 已批准，实施中；尚未完成生产接入与验收。日期：2026-10-20。

## 故障与证据

同步 coder 已产出最终文本，后台 Shell 仍 Working，sync 任务持续 11 小时未终结。现场时间线：2026-10-19 21:56:54 发起 Shell，21:57:32 前台超时 promote，22:03:17 child 最终回复，22:05:24 sync wrapper 转后台，22:07:33 loop 写 bounded handoff；之后没有 child Stop。特殊文件/FIFO 阻塞只是现场推断，未核实系统调用；修复覆盖任何永久 Working，不依赖此推断。不操作现场进程。

核查基线：`1247c7683e220e367c9f14396e06cbd9798a6649`。

- `session/subagent/child_runner.rs` 在 Completed 后等待 task/queue/activity，没有 deadline。
- `session/subagent/run_sync.rs` 的 120 秒只结束调用方等待，worker 继续运行；父投递失败另有无界重试。
- `agent/async_tasks/handoff.rs` 的旧写入以 0 混淆无任务、无能力及写失败。
- pool 持有 manager/MQ 不等于 worker 退出后具备可寻址取消、关闭及消息结算。

## 已批准契约

本任务以《Peri 子 Agent 无界收尾修复方案》（2026-10-20，A′，第 1–11 节）为验收契约。替代原 spec 的“timer → Completed”、仅限三个文件及 121 秒仍自动继续旧 child 的旧提案。

1. 首次 RCRA 推进结束且有未结任务/消息义务时启动唯一绝对 D，复用 `HANDOFF_MAX_WAIT=120s`；activity、loop 重入、短暂空闲不重置。
2. cancel 之后 Receive 之前、Receive 之后语义准入前检查 D；已准入 Reason/Act 完成配对，不中断其 future，不以 deadline 取消 Shell。
3. typed outcome 区分 Settled、HandedOff、Interrupted、Failed；receipt 包含 child/execution、接收方、原因、任务和消息引用、canonical ID。
4. HandedOff 映射 child Error、父 success=false、timed_out=false；保留文本及 pending，不伪造成功/Running。
5. 轻量 owner 接管原 manager/MQ、可信 scope 控制及 canonical 端口，仲裁 handoff/resume/close；不持有 LLM/StageContext/旧事件总线。
6. 冻结快照后 flush + idempotent append，再非 Active 状态提交、旧事件排空及一次 Stop/父结果；提交失败显式报告并保留资源，不无限等待。
7. 父结果 retry 交 registry 单一 worker，wrapper 释放完整 session；sync/bg × spawn/resume 同契约。
8. 父/根按 child/execution/task 在可信树内查询、取消、关闭。显式 close 非模型结算保留 MQ，失败返回 Incomplete，不删除唯一 owner。
9. 截止后需要显式 resume 才能再次推理；结果仍投直接 child。重启只加载已保存历史，不恢复旧执行或未落库 MQ。
10. 不新增 DB 表、持久状态枚举、持久 ACK 账本；不扩张 late result 即时 durable，不改 Shell promote/执行期限。

## 实施顺序

1. 定义 typed outcome、receipt、一次性 window，更新契约。
2. 轻量 owner 与 handoff/resume/close 准入。
3. 可靠提交、非 Active 状态与旧事件收尾。
4. child runner/loop 接入共享 D 与安全准入。
5. sync/background/spawn/resume、父结果及展示贯穿。
6. 移除 wrapper 自有投递等待、完成 close 消息结算与回归。

## 验收清单（未勾选均未完成）

- [ ] 永久 Working 无通知、每秒 activity、连续队列通知：D 不重置，计算退出但 Shell 仍 Working。
- [ ] 无任务正常退出；D 内完成由旧 child 正常处理；D 前/同刻两种调度/之后终态与 Stop 恰一次。
- [ ] active=0 但消息未消费仍交接；Receive 跨 D 保留历史；Reason 跨 D 的 Act 完成配对，不开下一轮。
- [ ] MaxIterations/HookStop 不追加模型预算，保留原错误与残留资源；cancel 与 D 同刻不混淆。
- [ ] sync/bg × 新建/resume、父注册与终态竞争、D 时 pending 空但安全单元未结束。
- [ ] callback 拒绝/panic/恢复由 registry 重试，wrapper 不驻留；无 owner/closing/注册失败显式报错。
- [ ] handoff append/status write 失败不误报交接或可 resume；资源可定位。
- [ ] late result 后显式 resume 复用原 MQ/manager、新执行身份，不重复 Shell。
- [ ] 不 resume 关闭根：可靠归档保留消息，确认资源后关闭，失败保留记录。
- [ ] 旧 bound 消息/Stop/cancel 不污染新 run；A→B→C 各层有界且直接归属不变。
- [ ] 重启仅加载历史；真实受控阻塞进程在 D 后仍存活，正式取消接口确认进程树及输出管道清理。

时间用例使用 paused-time；过滤测试须实际运行非零目标用例。至少运行 child_runner、run_sync、handoff 三组测试，以及新增模块定向测试、相关 build/clippy。真实 wire/进程验收不可由静态检查替代。

## 当前证据

- 基线 `agent::async_tasks::handoff::tests`：5 passed，0 failed；这是旧行为基线，不证明修复。
- 尚未接入生产 deadline；原无界等待问题仍存在。后续阶段不得将类型定义或窗口单测称为完整修复。
