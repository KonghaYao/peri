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

## 当前实现与交付限制

本分支仅完成基础类型和部分资源/提交路径，**不满足 A′ 验收，不可作为生产修复合入**。

- `412386e90`：阶段 1，定义 outcome/receipt、一次性窗口及目标契约。
- `ca20a81ec`：阶段 2 部分，父 TaskManager 保留轻量 owner，原 MQ/manager 复用、执行 permit、可信子树查询/取消与根关闭接入。
- `da969a92c`：阶段 3 部分，冻结交接快照、flush + 幂等 append、区分缺能力/写失败；同步状态提交失败显式返回，先提交再 Stop，事件排空失败阻止 owner resume。

仍须解决：

1. 阶段 2 尚未闭环：缺 owner 的嵌入路径没有拒绝跨 run 任务；owner 的准备/关闭竞态和后台注册失败释放待补；父/根按 execution 查询尚未实现。Close 暂不移除 owner/binding，且 retained Required 消息仍返回 Incomplete。
2. 阶段 3 尚未闭环：后台 status/Stop 顺序与事件排空失败时 owner 状态仍需统一；交接写失败快照保留已有基础，但没有完整修复/重试入口。`Failed` residual 尚未贯穿。
3. 阶段 4 未完成：`ChildSettlementWindow` 尚未接入 runner、loop 或 wrapper。尝试添加 `LoopResult::SettlementDue` 后发现需同步改造四条结果路径及 workflow 映射；该未完成接入已撤回，最终代码可编译。**原永久 Working 无限等待仍存在。**
4. 阶段 5–6 未完成：typed outcome 未用于运行路径，pending_message 仍为旧文案；wrapper 自有 retry 未移除；close 非模型归档未实现；121 秒旧行为测试未改。
5. 验收矩阵、真实进程/协议测试、deadline 前后/同刻竞态、计算对象释放、四路径最终行为均未验证。现有回归通过只说明基础改动未破坏这些旧用例。

## 实际验证

所有命令在指定 worktree 内执行，统一前缀 `./scripts/cargo-rmcp-patched.sh`。

- `test --locked -p peri-agent --lib -- session::subagent::child_runner::tests`：17 passed，0 failed（最终生产源码）。
- `test --locked -p peri-agent --lib -- session::subagent::run_sync::tests`：6 passed，0 failed（最终生产源码）。
- `test --locked -p peri-agent --lib -- agent::async_tasks::handoff::tests`：6 passed，0 failed，含新增 paused-time window 用例（最终生产源码）。
- `test --locked -p peri-agent --lib -- session::subagent::resource_owner::tests`：5 passed，0 failed（最终生产源码）。
- `test --locked -p peri-agent --lib -- session::subagent::tests::bound_and_tail_cases`：12 passed，0 failed（最终生产源码）。
- 阶段 2 的 `test --locked -p peri-agent --lib -- session::subagent::`：123 passed，0 failed；阶段 3 首轮 122 passed、1 failed（旧测试期待状态写入失败仍成功）；更新契约后定向通过。随后排空错误曾丢失 child 身份，定向 10 passed、2 failed；恢复 `SubagentFailure` 包装后 12 passed。
- 中途编译失败：新增 host 字段遗漏初始化、未导入 TaskManager trait、ACP 使用具体 manager 方法；均已修正。未完成 deadline 接入编译报四处 `LoopResult::SettlementDue` 非穷尽匹配，该接入已撤回。
- 提交 hook 的 check、clippy、fmt、layer-imports 已通过；第二个提交首轮被 fmt 拒绝，格式修正后正常提交，没有跳过 hook。
- `build --locked -p peri-agent -p peri-acp`：通过，退出码 0。
- `clippy --locked -p peri-agent -p peri-acp --lib -- -D warnings`：通过，退出码 0。
- `test --locked -p peri-agent --doc`：15 passed，0 failed。
- `test --locked -p peri-acp-types --doc`：6 passed，0 failed，2 ignored；第三方 patched hyper-util 编译报告未使用 timer 警告。
- 尚未运行最终全库测试、ACP 行为测试、真实 MCP wire 或受控进程测试；这些不是环境阻塞，而是实现/验证尚未完成。
