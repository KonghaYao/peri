# 工具边界 steer（阶段门）验收记录

- 状态：验收记录（分支 `fix/tool-boundary-steer`；PR 提交前定向验证）
- 日期：2026-11-22
- 基线：`origin/main` @ `1b7d3557b`（worktree 创建时的远端最新值）
- 工作树：`/Users/mino/code/peri/.worktrees/tool-boundary-steer`
- 范围：`UserInputMailbox` 的 `model_request_in_flight` 阶段门、`run_reason`
  实际 LLM 调用窗口的 RAII guard、`dispatch` 的取消门、attempt 生命周期复位、
  定向回归测试与契约文档。对应 issue
  `spec/issues/2026-11-22-p1-tool-boundary-steer.md`。
- 环境：macOS 26.6.2；`rustc`/`cargo` 1.99.0（`rust-toolchain.toml`）；
  构建统一经 `./scripts/cargo-rmcp-patched.sh`。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 1. 工具批次执行中立即发送：批次完成、同一 attempt 的下一次 Receive 摄取、不出现 Interrupted | 通过 | `test_run_react_loop_steer_during_tool_batch_is_received_next_iteration`：取消 token 未触发（`!cancel.is_cancelled()`）、`LoopResult::Completed`、ticket 仍有效、Reason 调用 2 次、mailbox 快照为空（两条均交付）；第二轮请求顺序为 工具结果 < `steer-1` < `steer-2` |
| 2. 实际 LLM 调用/流式中立即发送：仍硬取消，新 attempt 入口 Receive 摄取 | 通过 | `test_mailbox_dispatch_during_model_request_cancels_and_records_steer`；`peri-acp --lib steer_activation` 3 passed（既有流式 steer 用例：`steer_dispatch_cancelled_turn_keeps_activation_and_consumes_input` 等） |
| 3. Stop 任何阶段立即取消；未摄取输入回 Queued | 通过 | `test_mailbox_stop_during_model_request_cancels_immediately`、`test_mailbox_stop_outside_model_request_cancels_immediately`、`test_mailbox_soft_dispatch_then_stop_returns_input_to_queued` |
| 4. 普通待发仍 idle-only，不进入工具边界注入 | 通过（路径未改） | `test_mailbox_single_dispatch_preserves_ordinary_queue_until_natural_completion`、`test_mailbox_running_enqueue_never_wakes_consumption_queue`（均在 mailbox 60 passed 内） |
| 5. 多条 steer 同一 Receive 批次按 MQ 顺序进入上下文 | 通过 | 上表第 1 项的顺序断言；`test_mailbox_dispatch_snapshot_and_consecutive_commands_preserve_acceptance_order`（接受顺序仍按服务端 `(enqueued_at,id)`） |
| 6. 目标测试、format、定向 clippy 通过 | 通过 | 见第 2 节命令与结果 |

阶段门生命周期复位（防止 guard 泄漏导致后续阶段误取消）由
`test_attach_attempt_clears_model_request_flag`、`test_finish_attempt_clears_model_request_flag`、
`test_fail_reserved_clears_model_request_flag` 覆盖；窗口外发送只移交 MQ 的用例为
`test_mailbox_dispatch_outside_model_request_hands_off_without_cancel`。

## 2. 运行命令与结果

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib user_input_mailbox` | 60 passed / 0 failed（1537 filtered out） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib loop_lifecycle` | 7 passed / 0 failed（1590 filtered out） |
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

## 4. 未覆盖与证据边界

- 未跑 `--workspace --all-targets` 全量测试与 CI 矩阵（`.github/workflows/ci.yml`）；
  本记录只覆盖上表定向范围，最终以 PR CI 为准。
- 未做真实终端手工交互（TUI 点击“立即发送”），未运行外置 `peri-e2e`
  的 `tests/smoke/steer-queue-live.test.ts`（不在本仓库）；列为后续对账项。
- 阶段门边界竞态未做确定性复现，仅作为行为边界记录：传入恰好发生在 Reason
  即将结束/guard 释放前会硬取消一次（输入安全，由新 attempt 入口摄取）；发生在
  Receive 选择之后、工具批次开始前，则需等到下一个工具边界（最多一轮迭代）。
- 假定同一 mailbox 同时只有一个 active attempt（当前宿主保证）；若未来并发共享
  mailbox，需要改为 per-ticket 门。
- 子 Agent 的 `user_input_mailbox == None`，guard 为 no-op；该路径由类型结构保证，
  未单独设用例。
- 验证完成后 `origin/main` 前进 1 个提交（`1cbf7141d`，PR #523），与本分支改动
  行无重叠；PR 的合并结果由 CI 覆盖，本记录不宣称已在新基线上复跑。
