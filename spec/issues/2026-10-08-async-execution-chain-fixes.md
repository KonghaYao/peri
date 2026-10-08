# 当前进程异步执行链独立修复

- 日期：2026-10-08
- 状态：实现及轻量验证完成，待用户真实使用验收
- 基线：`554b324d`
- 工作区：`/Users/konghayao/code/ai/peri-async-chain-fixes-20261008`
- 权威契约：`docs/design/session-async-tasks.md`、`docs/design/rcra-message-activation.md`

## 授权及边界

修复 Astra 静态审查识别的三个 P1 与两个 P2。已有 retained wake 与空跑准入修复保留。主工作树有其他任务未提交改动，不复制、不暂存、不清理，也不自动合并本批次。SDK 不改，不恢复已删除的 WorkState、持久 inbox、执行恢复或终态 ACK 账本。

调查的交错是源码推导，不冒充动态复现；本任务通过定向测试核实。未证明的 reload/listener、broadcast 负载丢失、全局锁性能风险不扩大到本批次。

## 数据流与责任

输入接纳由 mailbox 负责；宿主负责 root 的执行准入、串行化、当前交互身份和自动续跑；RCRA 负责一次计算推进；child runner 保留当前进程内的委派完成责任；TaskManager 负责终态结算；transcript 是历史权威；TUI lifecycle 管理反向交互许可，bridge 只投影所属执行状态。

## 工作拆分

### A：Child 执行与终态 owner

写集：`peri-agent/src/session/subagent/` 及其直接测试，必要的挂载在 `peri-agent/src/session/subagent.rs`。

1. Forwarder 错误不能跳过终态结算。原模型/取消失败原因仍为权威，转发失败明确可见。
2. Bound 到界且有未完成子任务时，不向父报告虚假完成；保留在进程内继续处理自己 MQ 的责任。
3. 保留既有取消、关闭与语义迭代预算，不以延长 timeout 或禁用 bounded idle 代替修复。当前 Task 工具无独立整任务 wall-clock timeout，120 秒仅是一轮 RCRA 的 idle bound，不宣称本批次补充总任务超时。
4. 无当前待处理任务时正常完成，重复结果不重复发布终态。

定向验证：已有 forwarder panic 分支改为失败终态/active 归零；新增嵌套 child 越界后晚到结果与取消测试。

### B：宿主交互生命周期与可判序挂起

写集：`peri-acp/src/host/`、`peri-acp/src/event/`、`peri-acp/src/session/event_sink*`；必要的事件契约与 Agent suspend 发布路径。

1. 内部 continuation 分配独立非空身份；可靠开始通知先于权限请求/提问，终态匹配相同身份。
2. 定时任务的执行前审批有独立交互作用域，并在批准、拒绝和失败后准确收尾。
3. 执行等待与输出的顺序在事件发布处解决，不让旧 suspend 越过新输出。
4. 保留现有用户输入协议与来源准入，错误不吞掉，不新增全局 loading 兜底。

定向验证：宿主通知顺序与身份；原取消/关闭/晚到结果回归；审批作用域收尾。

### C：TUI 交互许可与视图归属

写集：`peri-tui/src/acp_client/` 与 `peri-tui/src/kit/` 的相关生命周期/输入/notifier及测试。

1. 消费宿主内部开始事件，建立可关闭且不会跨会话的 HITL lease。
2. 保留终态身份，旧终态不能结算当前新执行；重复开始不重复发布。
3. 迟到提交失败携带原 session/request 身份；移除日常错误路径的无归属全局 reset。
   managed 输入与 external execution 必须区分，external 的 Stop 不得伪装成不存在的 mailbox 票据。
4. 与宿主有序 suspend 配合，保留普通用户、history load/replay、关闭的既有行为。

定向验证：内部开始后 permission/elicitation 可响应；matching done 释放许可；旧 done、旧提交失败、会话切换不影响当前执行。

## 协作规则

各 worker 在上述同一独立 worktree 直接修改自己写集，不 commit、不操作主树、不再委派。协议增量由主代理先确定并通知 TUI worker；worker 不修改其他写集或文档。测试命令由主代理统一运行，防止共享 target 并发 Cargo 争用。单源文件不超过 1000 行；删除错误状态机优于补布尔兼容锁。

## 集成与轻量验收

1. 审核各写集、调用关系与错误路径，核实未引入持久执行机制。
2. 精确过滤 worker 新增的少量生命周期测试，再跑原 host activation/continuation 回归。
3. 格式、diff、修改文件尺寸及依赖边界检查；涉及公开文档示例时运行相应 doc tests。
4. 记录真实通过数量、失败/既有问题、未覆盖的真实使用验证。
5. 仅在本 worktree 提交已验证完整快照，不自动合并主工作树；上层整合需适配其他任务的未提交改动。

## 实施记录

- 两个 sol/xhigh worker 分别实现 child 与 TUI，主代理完成宿主、事件契约及整合；worker 不独立提交，也不并发运行 Cargo。
- `child_runner.rs` 由 foreground 委派 future 或 TaskManager 所有的 background task 保留消费责任；单轮 bound 到界后继续等待本 child 的 required MQ、任务目录活动及取消，再按剩余语义预算推进。没有持久恢复、root 转投或额外超时延长。
- Forwarder/terminal bridge 失败在真实资源已关闭后继续发布可见失败终态、结算任务和注销 runtime；既有模型失败与取消原因保持权威。
- `host/execution.rs` 可靠发送实际身份开始事件，并对装配早退收尾。定时审批持有 prompt lock，开始先于 permission，支持取消/关闭，审批与后续执行使用不同身份。
- `TurnSuspended` 从 state 通道移到 render FIFO，删除原内部变体；child forwarder 仍过滤自己的挂起，不让子任务控制父 loading。
- TUI 用实际身份建立反向交互许可，保留 done/失败的会话及请求归属；已退役或不匹配的结束不结算新执行。整合额外补齐 managed/external Stop 区分与输入回滚记录的请求归属，防止不存在的票据取消被忽略、内部审批/执行误回滚待执行输入。

## 已验证范围

全部通过的定向行为测试共有 **61 个不同测试**，重复运行不重复计数；未命中的过滤器不计为验证。所有命令使用 patched Cargo wrapper、`--locked --offline`，Cargo 串行执行。

| 范围 | 精确过滤器或入口 | 通过数 |
| --- | --- | --- |
| Child owner | `session::subagent::child_runner::tests` | 4 |
| Forwarder 与 terminal bridge 故障 | `session::subagent::tests::bound_and_tail_cases::test_spawn_subagent_background_forwarder_panic`、`test_spawn_subagent_sync_forwarder_panic_is_failure`、`test_spawn_subagent_terminal_bridge_panic_is_failure` | 5 |
| Child suspend 过滤与 idle 输入队列 | `agent::subagent_event_forwarder::tests::test_forwarder_filters_turn_committed`、`agent::stages::tests::loop_lifecycle_tests::test_run_react_loop_idle_dispatches_queued_prompts_one_at_a_time` | 2 |
| 宿主原激活与新执行身份 | `host::requests::tests::activation_tests` | 9 |
| 原 continuation 准入、取消及关闭 | `host::continuation::tests` | 12 |
| 宿主 suspend/output FIFO | `event::forwarder_test::test_suspended_then_resumed_output_remains_fifo_when_forwarder_is_delayed` | 1 |
| TUI lease、generation、旧 owner 及 external Stop | `acp_client::client::pump::user_input_run_tests` | 11 |
| managed Stop 身份 | `acp_client::client::steer::tests::test_user_input_stop_preserves_managed_run_identity_on_wire` | 1 |
| Bridge 旧终态/迟到失败/回滚归属 | `kit::acp_events::acp_events_test` 中三个新定向测试；`kit::acp_bridge::tests::test_late_prompt_failure_from_previous_session_does_not_reset_loading` | 4 |
| 既有取消与正常回滚清理 | `kit::acp_events::acp_events_test::turn_interrupted_test`、`turn_archive_test::test_turn_done_clears_last_submitted_text` | 10 |
| notifier 身份与 execution 投影 | `kit::acp_notifier::execution_done_test`、`kit::acp_notifier::agent_event::tests::test_execution_started_projects_loading_with_actual_request_identity` | 2 |

- ACP lib check 通过；`peri-acp-types` doc tests：1 通过、2 既有 ignored；`peri-agent` doc tests：10 通过。ACP 与 TUI doc test 命令完成，但当前无可执行示例，0 tests 不计为通过数量。共 61 个定向行为测试及 11 个文档测试通过。
- 格式、修改 Rust 文件尺寸、diff 与 22 条全边依赖门检查通过；提交前再次核对最终快照。
- 未运行全量 workspace tests、完整 pre-commit/check/clippy hooks 或真实模型/TUI E2E；独立提交采用显式跳过 hooks，保留以上轻量证据。日志位于本 worktree 的 `target/async-chain-*.log`，不纳入源码提交。
- 曾有两个测试过滤器未命中；均已按实际挂载路径重跑，0 tests 不算通过。

## 局限及待验收

- 本批次不证明 listener panic 后重建、broadcast 过载丢失、全局 sessions lock 性能或 scheduler 重复 waiters 的额外风险已经解决。
- 原 explicit close 资源关闭失败仍保留 active 并暂不发布委派终态；本批次不新增关闭重试 owner，也不伪造资源已停止。
- 同一 session 离开后再选回，若旧 `ExecutionStarted` 比任何 snapshot 更早到达，仅 mailbox generation 不能证明它属于本次选中；若需严格排除，宿主选中响应还需提供期望 generation，本批次未扩展该响应。
- 用户验收：实际嵌套异步任务超过单轮 bound 后仍续跑；内部首步 permission/ask 可响应；空闲定时审批及 Stop 可收尾；切换会话后旧失败不清除新 loading；history 会话加载与普通输入保持可用。
- 本工作树单独提交、不自动合并主树。独立修复基线为 `554b324d`；开始检查时主树有 152 个其他任务修改路径，本批次未操作这些改动。收尾时另一任务已将主树推进至 `45a7a5d6`（context preflight C 整合），主树状态干净；两树公共基线仍是 `554b324d`，本修复未合并其中。
