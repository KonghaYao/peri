# 工具边界 steer（阶段门）验收记录

- 状态：验收记录（分支 `fix/tool-boundary-steer`；PR 提交前定向验证）
- 日期：2026-11-22
- 基线：`origin/main` @ `1b7d3557b`（worktree 创建时的远端最新值）
- 工作树：`/Users/mino/code/peri/.worktrees/tool-boundary-steer`
- 范围：`UserInputMailbox` 的 `model_request_in_flight` 阶段门、`run_reason`
  实际 LLM 调用窗口的 RAII guard、`dispatch` 的取消门、attempt 生命周期复位、
  定向回归测试与契约文档。原过程记录已归 [11 月月志](../../spec/history/2026-11.md)；此记录只对应上列历史基线，不代替 [#569](https://github.com/KonghaYao/peri/pull/569) 的后续改动或验证。
- 环境：macOS 26.6.2；`rustc`/`cargo` 1.99.0（`rust-toolchain.toml`）；
  构建统一经 `./scripts/cargo-rmcp-patched.sh`。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 1. 工具批次执行中立即发送：批次完成、同一 attempt 的下一次 Receive 摄取、不出现 Interrupted | 通过 | `test_run_react_loop_steer_during_tool_batch_is_received_next_iteration`：取消 token 未触发（`!cancel.is_cancelled()`）、`LoopResult::Completed`、ticket 仍有效、Reason 调用 2 次、mailbox 快照为空（两条均交付）；第二轮请求顺序为 工具结果 < `steer-1` < `steer-2` |
| 2. 实际 LLM 调用/流式中立即发送：仍硬取消，新 attempt 入口 Receive 摄取 | 通过 | `test_mailbox_dispatch_during_model_request_cancels_and_records_steer`；`test_run_reason_error_hook_observes_closed_steer_gate`（`on_error` 时窗口已关闭）；`peri-acp --lib steer_activation` 3 passed（既有流式 steer 用例：`steer_dispatch_cancelled_turn_keeps_activation_and_consumes_input` 等）。移交确认迟到、输入已被摄取时不再取消：`test_late_handoff_confirmation_does_not_cancel_consuming_request` |
| 3. Stop 任何阶段立即取消；未摄取输入回 Queued | 通过 | `test_mailbox_stop_during_model_request_cancels_immediately`、`test_mailbox_stop_outside_model_request_cancels_immediately`、`test_mailbox_soft_dispatch_then_stop_returns_input_to_queued`；Stop 收尾中的重新发送保持显式继续（`test_mailbox_dispatch_after_stop_marks_explicit_continuation`）；复位与并发 dispatch 的接纳/撤回有序（`test_stop_reset_and_concurrent_dispatch_leave_record_reselectable`） |
| 4. 普通待发仍 idle-only，不进入工具边界注入 | 通过（路径未改） | `test_mailbox_single_dispatch_preserves_ordinary_queue_until_natural_completion`、`test_mailbox_running_enqueue_never_wakes_consumption_queue`（均在 mailbox 61 passed 内） |
| 5. 多条 steer 同一 Receive 批次按 MQ 顺序进入上下文 | 通过 | 上表第 1 项的顺序断言；`test_mailbox_dispatch_snapshot_and_consecutive_commands_preserve_acceptance_order`（接受顺序仍按服务端 `(enqueued_at,id)`） |
| 6. 目标测试、format、定向 clippy 通过 | 通过 | 见第 2 节命令与结果 |

阶段门生命周期复位（防止 guard 泄漏导致后续阶段误取消）由
`test_attach_attempt_clears_model_request_flag`、`test_finish_attempt_clears_model_request_flag`、
`test_fail_reserved_clears_model_request_flag` 覆盖；窗口外发送只移交 MQ 的用例为
`test_mailbox_dispatch_outside_model_request_hands_off_without_cancel`。

窗口边界收紧（首轮 CI/评审后）：

- `run_reason` 的 select 返回即释放 guard，`run_on_error` 等错误处理不再落在窗口内；
  由 `test_run_reason_error_hook_observes_closed_steer_gate` 断言 `on_error` 时窗口为关。
- `dispatch` 的阶段门判定、`reason=Steer` 与 `cancel()` 在同一 mailbox 锁临界区完成，
  guard 的 `Drop` 不能插在判定与取消之间。
- Stop 收尾中的重新发送保持既有“显式继续”语义：令牌已由 Stop 取消，不重复取消，
  但把 `reason` 从 Stop 改为 Steer，让宿主保留续跑激活；由
  `test_mailbox_dispatch_after_stop_marks_explicit_continuation` 覆盖。
- 取消判定以「接纳是否已被 Receive 摄取」为准（第三轮评审后）：`Claimed/Delivered`
  的输入已进入当次请求上下文，不再取消；只有仍 `Dispatching` 的接纳才硬取消。
  由 `test_late_handoff_confirmation_does_not_cancel_consuming_request` 覆盖。
- 记录复位（Stop/失败收尾）与 `withdraw_send_inputs` 收进同一 mailbox 临界区；
  `dispatch`/`send_ready_ids` 记录接纳时若看到记录已回到 `Queued`，不写 `handed_off`
  并在同一临界区撤回自身选择。复位前已 promote 的 `dispatch` 以 promote 为线性化点
  输给收尾，记录保持 `Queued` 可重新发送；复位后才 promote 的接纳不受撤回影响。
  由 `test_stop_reset_and_concurrent_dispatch_leave_record_reselectable` 覆盖。

## 2. 运行命令与结果

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib` | 1596 passed / 0 failed / 5 ignored（全量 lib，第三轮修复后） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib user_input_mailbox` | 63 passed / 0 failed（1538 filtered out；含两条新增竞态用例） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib loop_lifecycle` | 7 passed / 0 failed（1590 filtered out） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib receive::tests::test_receive_user_input_delivery_keeps_identity_order_and_render_fifo` | 1 passed / 0 failed（首轮 CI 失败用例） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib steer_activation` | 3 passed / 0 failed（1508 filtered out） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- steer` | 108 passed / 0 failed（2698 filtered out；UI 契约回归） |
| `bash scripts/check-changed.sh` | 通过（`cargo check --locked` 反向依赖闭包 14 个 crate；2m59s） |
| `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-agent -p peri-acp --all-targets -- -D warnings` | 通过 |
| `cargo fmt --check` | 无差异 |
| `git diff --check` | 无输出 |

## 3. 失败与复跑记录

1. **首轮编译失败**：`loop_lifecycle_test.rs:345` 缺 `CancellationToken` 导入
   （E0433），`user_input_mailbox.rs:34` 的 `pub(crate) use ModelRequestGuard`
   未被使用（warning）。已补导入、移除多余 re-export；后续全部通过。
2. **mailbox 首轮 59/60**：`test_mailbox_dispatch_snapshot_and_consecutive_commands_preserve_acceptance_order`
   在 `reserve_run()` 处 panic。原用例默认「dispatch 必然取消」，而新语义下
   `Interrupted` + `reason=None` 按失败回收置 `paused=true`，预留返回 `None`。
   该用例验证的是发送选择的接受顺序，已改为在 `begin_model_request()` 窗口内
   执行 dispatch（与 `test_mailbox_steer_then_late_idle_preserves_selected_continuation`
   的改法一致），复跑 60/60。
3. **环境失败（非产品缺陷）**：coder 首轮测试因 `curl: (6) Could not resolve host:
   static.crates.io`（exit 6）失败，未产生代码结论。本机 `static.crates.io` 与
   `rsproxy.cn` 的静态重定向域均不可解析，故使用预置的 patched crate 目录与
   自定义 `CARGO_HOME`（仅索引替换）完成构建；依赖版本与 feature 未改变。
4. **TUI 测试链接器提示**：`ld: __eh_frame section too large ...` 为链接器性能
   提示，非编译/lint 错误；同次 108 passed。
5. **首轮 PR CI（ubuntu）失败**：`agent::stages::receive::tests::test_receive_user_input_delivery_keeps_identity_order_and_render_fifo`
   在 `reserve_run()` panic。原因是该既有用例同样默认「dispatch 必然取消」，
   `Interrupted` + `reason=None` 触发失败回收置 `paused=true`。已改为在
   `begin_model_request()` 窗口内执行两次 dispatch，复跑该用例并通过全量 lib
   （1593 passed）。
6. **首轮 CodeRabbit 两条 Minor 意见**：①错误处理仍在 guard 内——已改为 select
   返回即释放 guard，并新增 `test_run_reason_error_hook_observes_closed_steer_gate`；
   ②阶段门判定与 `cancel()` 之间存在 guard 可插入的间隙——已把取消并入同一
   临界区。两条均按建议收紧，未改产品语义。
7. **pre-commit `tui-components` 环境失败**：系统 `python3` 为 3.9（缺 `tomllib`）；
   改用 `python3.12` 置于 PATH 后 8 个用例与边界检查全部通过，非代码问题。
8. **第二轮 CodeRabbit Minor 意见（Stop 后重新发送）**：阶段门使 Stop 收尾中的重新发送
   不再记录 Steer，宿主会走 `activation.suppress()`，与既有“Stop 后重新发送是明确继续”
   契约冲突。已保留原语义：令牌由 Stop 取消，不重复取消；仅把 `reason` 改为 Steer。
   新增 `test_mailbox_dispatch_after_stop_marks_explicit_continuation`，mailbox 61/61。
9. **第三轮 CodeRabbit 两条 Minor 意见**：①`dispatch` 只看阶段门，移交确认迟到时会取消
   已经摄取该输入的请求——已改为按记录状态判定（`Claimed/Delivered` 不取消），并补
   `test_late_handoff_confirmation_does_not_cancel_consuming_request`；②Stop 复位先收集 ID、
   释放锁后才撤回，并发的 `dispatch` 接纳可能被撤回成「`Queued` 且已挑选」——已把
   复位与撤回收进同一临界区，接纳侧发现记录已回 `Queued` 时不写 `handed_off` 并撤回
   自身选择，补 `test_stop_reset_and_concurrent_dispatch_leave_record_reselectable`。
   其余既有用例全部复跑通过（全量 lib 1596 passed）。

## 4. 未覆盖与证据边界

- 未跑 `--workspace --all-targets` 全量测试与 CI 矩阵（`.github/workflows/ci.yml`）；
  本记录只覆盖上表定向范围，最终以 PR CI 为准。
- 未做真实终端手工交互（TUI 点击“立即发送”），未运行外置 `peri-e2e`
  的 `tests/smoke/steer-queue-live.test.ts`（不在本仓库）；列为后续对账项。
- 阶段门边界竞态未做确定性复现，仅作为行为边界记录：传入恰好发生在 Reason
  即将结束/guard 释放前会硬取消一次（输入安全，由新 attempt 入口摄取）；发生在
  Receive 选择之后、工具批次开始前，则需等到下一个工具边界（最多一轮迭代）。
  移交确认迟到与 Stop 复位/并发接纳两条竞态已用可控时序用例覆盖（见 3.9）；
  「复位前已 promote 的 dispatch 以 promote 为线性化点输给收尾」属于裁决规则，
  未穷举所有指令交错。
- 假定同一 mailbox 同时只有一个 active attempt（当前宿主保证）；若未来并发共享
  mailbox，需要改为 per-ticket 门。
- 子 Agent 的 `user_input_mailbox == None`，guard 为 no-op；该路径由类型结构保证，
  未单独设用例。
- 验证完成后 `origin/main` 前进 1 个提交（`1cbf7141d`，PR #523），与本分支改动
  行无重叠；PR 的合并结果由 CI 覆盖，本记录不宣称已在新基线上复跑。

## 5. 5.3.1 工具后首个模型请求回归

基线为发布标签 `agent-v5.3.1`（`a6a6ceec0`），已含 #548。以下两条测试使用真实的
Act、MQ、Receive 与请求准备流程，由可控工具和模型记录实际交给模型的消息。

- 在工具阻塞期间先放入 130 条后台进度，再显式发送两条输入。每次 Receive 最多摄取
  64 条；旧循环在第一页之后就发起模型请求，新指令要等更晚的调用。
- 工具返回后的 Receive 已完成，在第二次 `before_model` 暂停时显式发送。旧请求缺少新指令。
  将语义迭代预算固定为两次，旧版两条用例都返回 `MaxIterationsExceeded(2)`。

命令为 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib test_tool_boundary_steer`。
断言覆盖同一 attempt 完成、恰好两次调用、工具结果先于两条按序且唯一的用户输入，
以及普通待发仍留在队列。可控模型验证请求内容；模型如何理解指令取决于所用模型。
